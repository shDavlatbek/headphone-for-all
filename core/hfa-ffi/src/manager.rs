//! The global engine manager behind the flutter_rust_bridge API.
//!
//! One [`EngineManager`] per process ([`manager`], `OnceLock`) owns:
//! - a tokio multi-thread runtime (engines, event forwarders, discovery),
//! - the loaded [`Settings`] and [`Identity`] (`init_app`),
//! - at most one [`HubHandle`] and one [`SenderHandle`], and one discovery task,
//! - the Dart event subscriptions (hub events and sender status), which outlive engine
//!   restarts: a forwarder task per running engine relays its `broadcast` channel to them.
//!
//! Trust is never cached: the engines load their own [`TrustStore`] from
//! `settings.data_dir` and save the pairings they make, so every trust read or change here
//! (`trusted_peers`, `forget_peer`, `sender_start` key pinning, the discovery `trusted` flag)
//! loads `trusted.json` afresh. A cached copy would miss pairings made since `init_app`, and
//! saving it (`forget_peer`) would erase them.
//!
//! Event order: a forwarder is subscribed to an engine's channel right after the engine
//! starts, before anything else, and on stop it is drained (until the engine's channel
//! closes, bounded by [`FORWARDER_DRAIN`]) before the final idle status is sent, so that
//! status is always the last one a subscriber sees.
//!
//! Locking: `lifecycle` serializes the operations that may block (init, start, stop) so
//! that `state` is only ever held for short, non-blocking sections; getters therefore never
//! wait for a hub or capture that is starting. Engine futures are driven with
//! `Runtime::block_on` from the calling flutter_rust_bridge worker thread.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use hfa_core::config::SETTINGS_FILE;
use hfa_core::{
    HubConfig, HubEngine, HubEvent, HubHandle, Identity, SenderConfig, SenderEngine, SenderEvent,
    SenderHandle, SenderState, Settings, TrustStore,
};
use parking_lot::Mutex;
use tokio::runtime::Runtime;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::api::app::{AppInfo, SettingsDto, TrustedPeerDto};
use crate::api::hub::{HubEventDto, HubStatusDto, PairingInfoDto, SourceDto};
use crate::api::sender::{DiscoveryEventDto, SenderStartDto, SenderStatusDto};
use crate::convert;
use crate::error::{FfiError, Result};
use crate::feeds;
use crate::hub_target::{self, HubRequest};
use crate::runtime::{block_on, build_runtime};
use crate::sender_meta::SenderMeta;

/// Device buffer size asked from the hub output (ms).
pub const OUTPUT_BUFFER_MS: u32 = 20;

/// How long a stopping engine's event forwarder may take to relay the events still queued
/// (the engine's channel closes once its tasks have ended) before it is aborted.
pub(crate) const FORWARDER_DRAIN: Duration = Duration::from_millis(500);

/// A destination for events delivered to Dart (a flutter_rust_bridge `StreamSink`, or a
/// test double).
pub(crate) trait EventSink<T>: Send + Sync {
    /// Delivers `value`. Returns `false` once the receiver is gone; the sink is then
    /// dropped (which closes the Dart stream).
    fn deliver(&self, value: T) -> bool;
}

impl<T> EventSink<T> for crate::frb_generated::StreamSink<T>
where
    T: crate::frb_generated::SseEncode + Send + Sync,
{
    fn deliver(&self, value: T) -> bool {
        self.add(value).is_ok()
    }
}

/// A set of subscriptions receiving the same events.
pub(crate) struct SinkSet<T> {
    sinks: Mutex<Vec<Box<dyn EventSink<T>>>>,
}

impl<T: Clone> SinkSet<T> {
    fn new() -> Self {
        Self {
            sinks: Mutex::new(Vec::new()),
        }
    }

    /// Adds a subscription.
    fn add(&self, sink: Box<dyn EventSink<T>>) {
        self.sinks.lock().push(sink);
    }

    /// Delivers `initial()` to `sink` and adds it, both under the set's lock, so that a
    /// concurrent [`SinkSet::broadcast`] either happened before `initial()` was computed (and
    /// is reflected in it) or is delivered after it: no change falls in between. A sink that
    /// refuses the initial value is not kept. `initial` must not use this set.
    fn add_with_initial(&self, sink: Box<dyn EventSink<T>>, initial: impl FnOnce() -> T) {
        let mut sinks = self.sinks.lock();
        if sink.deliver(initial()) {
            sinks.push(sink);
        }
    }

    /// Delivers `value` to every subscription, dropping the closed ones.
    fn broadcast(&self, value: &T) {
        self.sinks.lock().retain(|s| s.deliver(value.clone()));
    }

    /// [`SinkSet::broadcast`], then runs `then` before any other broadcast or new
    /// subscription can happen. `then` must not use this set.
    fn broadcast_then<R>(&self, value: &T, then: impl FnOnce() -> R) -> R {
        let mut sinks = self.sinks.lock();
        sinks.retain(|s| s.deliver(value.clone()));
        then()
    }

    /// Number of live subscriptions.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.sinks.lock().len()
    }
}

/// What `init_app` loaded (trust is loaded per use; see the module docs).
struct AppContext {
    settings: Settings,
    identity: Identity,
}

/// The status to send for a sender's folded events.
fn meta_dto(meta: &SenderMeta) -> SenderStatusDto {
    convert::sender_status_dto(
        &meta.status,
        meta.hub_name.as_deref(),
        meta.last_error.as_deref(),
    )
}

struct HubSlot {
    handle: HubHandle,
    forwarder: JoinHandle<()>,
}

struct SenderSlot {
    handle: SenderHandle,
    forwarder: JoinHandle<()>,
    meta: Arc<Mutex<SenderMeta>>,
    feed_id: Option<u32>,
}

#[derive(Default)]
struct State {
    app: Option<AppContext>,
    hub: Option<HubSlot>,
    sender: Option<SenderSlot>,
    discovery: Option<JoinHandle<()>>,
}

/// Owner of the runtime, the loaded identity and the running engines. See the module docs.
pub(crate) struct EngineManager {
    runtime: Runtime,
    lifecycle: Mutex<()>,
    state: Mutex<State>,
    hub_sinks: Arc<SinkSet<HubEventDto>>,
    sender_sinks: Arc<SinkSet<SenderStatusDto>>,
}

/// The process-wide manager, created on first use.
///
/// # Errors
/// [`FfiError::Internal`] if the tokio runtime cannot be created.
pub(crate) fn manager() -> Result<&'static EngineManager> {
    static MANAGER: OnceLock<EngineManager> = OnceLock::new();
    if let Some(m) = MANAGER.get() {
        return Ok(m);
    }
    let fresh = EngineManager::new()?;
    // If another thread won the race, `fresh` is dropped here (outside any runtime).
    Ok(MANAGER.get_or_init(|| fresh))
}

impl EngineManager {
    /// Creates a manager with its own runtime and no loaded app.
    pub(crate) fn new() -> Result<Self> {
        Ok(Self {
            runtime: build_runtime("hfa-ffi")?,
            lifecycle: Mutex::new(()),
            state: Mutex::new(State::default()),
            hub_sinks: Arc::new(SinkSet::new()),
            sender_sinks: Arc::new(SinkSet::new()),
        })
    }

    // ---------------------------------------------------------------- app

    /// See `api::app::init_app`.
    pub(crate) fn init_app(
        &self,
        data_dir: &Path,
        first_run_name: Option<&str>,
    ) -> Result<AppInfo> {
        let _ops = self.lifecycle.lock();
        crate::logging::init_tracing();
        {
            let st = self.state.lock();
            if let Some(app) = &st.app {
                if app.settings.data_dir == data_dir {
                    return Ok(app_info(app));
                }
                if st.hub.is_some() || st.sender.is_some() {
                    return Err(FfiError::InvalidArgument(
                        "cannot switch data_dir while a hub or sender is running".to_owned(),
                    ));
                }
            }
        }
        if data_dir.as_os_str().is_empty() {
            return Err(FfiError::InvalidArgument("data_dir is empty".to_owned()));
        }
        std::fs::create_dir_all(data_dir)?;
        let first_run = !data_dir.join(SETTINGS_FILE).exists();
        let mut settings = Settings::load_or_default(data_dir)?;
        settings.data_dir = data_dir.to_path_buf();
        if first_run {
            if let Some(name) = first_run_name.map(str::trim).filter(|n| !n.is_empty()) {
                name.clone_into(&mut settings.device_name);
            }
            settings.save()?;
        }
        let identity = Identity::load_or_create(data_dir, &settings.device_name)?;
        // Not kept (see the module docs), but a corrupt trust store fails early here.
        TrustStore::load(data_dir)?;
        let app = AppContext { settings, identity };
        let info = app_info(&app);
        tracing::info!(
            device_id = %info.device_id,
            data_dir = %data_dir.display(),
            "headphone-for-all initialized"
        );
        self.state.lock().app = Some(app);
        Ok(info)
    }

    /// See `api::app::get_settings`.
    pub(crate) fn get_settings(&self) -> Result<SettingsDto> {
        let st = self.state.lock();
        let app = st.app.as_ref().ok_or(FfiError::NotInitialized)?;
        Ok(convert::settings_dto(&app.settings))
    }

    /// See `api::app::update_settings`.
    pub(crate) fn update_settings(&self, dto: SettingsDto) -> Result<()> {
        let _ops = self.lifecycle.lock();
        let mut st = self.state.lock();
        let app = st.app.as_mut().ok_or(FfiError::NotInitialized)?;
        let settings = convert::apply_settings(&app.settings, &dto)?;
        settings.save()?;
        app.identity.name.clone_from(&settings.device_name);
        app.settings = settings;
        Ok(())
    }

    /// See `api::app::trusted_peers`.
    pub(crate) fn trusted_peers(&self) -> Result<Vec<TrustedPeerDto>> {
        let trust = self.trust()?;
        Ok(trust
            .peers()
            .iter()
            .map(convert::trusted_peer_dto)
            .collect())
    }

    /// See `api::app::forget_peer`.
    pub(crate) fn forget_peer(&self, device_id: &str) -> Result<()> {
        let trust = self.trust()?;
        if !trust.remove(device_id)? {
            tracing::debug!(device_id, "forget_peer: not a trusted peer");
        }
        Ok(())
    }

    /// The trust store as currently saved (never a cached copy; see the module docs).
    fn trust(&self) -> Result<TrustStore> {
        Ok(TrustStore::load(&self.data_dir()?)?)
    }

    fn data_dir(&self) -> Result<PathBuf> {
        let st = self.state.lock();
        let app = st.app.as_ref().ok_or(FfiError::NotInitialized)?;
        Ok(app.settings.data_dir.clone())
    }

    // ---------------------------------------------------------------- hub

    /// See `api::hub::hub_start`.
    pub(crate) fn hub_start(&self) -> Result<HubStatusDto> {
        let _ops = self.lifecycle.lock();
        let settings = {
            let st = self.state.lock();
            let app = st.app.as_ref().ok_or(FfiError::NotInitialized)?;
            if st.hub.is_some() {
                drop(st);
                return Ok(self.hub_status());
            }
            app.settings.clone()
        };
        // cpal's AAudio output panics without the context set by `NativeBridge.init`.
        #[cfg(target_os = "android")]
        crate::android::ensure_audio_context()?;
        let output = hfa_capture::open_output(&settings.output, OUTPUT_BUFFER_MS)?;
        let handle = block_on(
            &self.runtime,
            HubEngine::start(HubConfig {
                settings,
                output,
                advertise: true,
            }),
        )??;
        // Subscribe before anything else so that no early event is lost.
        let forwarder = self.runtime.spawn(forward_hub_events(
            handle.events(),
            Arc::clone(&self.hub_sinks),
        ));
        tracing::info!(port = handle.local_port(), "hub started");
        self.state.lock().hub = Some(HubSlot { handle, forwarder });
        Ok(self.hub_status())
    }

    /// See `api::hub::hub_stop`.
    pub(crate) fn hub_stop(&self) -> Result<()> {
        let _ops = self.lifecycle.lock();
        let slot = self.state.lock().hub.take();
        if let Some(slot) = slot {
            let stopped = block_on(&self.runtime, slot.handle.stop());
            // Relays the last source updates, then ends (the engine's channel is closed).
            block_on(
                &self.runtime,
                finish_forwarder(slot.forwarder, FORWARDER_DRAIN),
            )?;
            stopped?;
            tracing::info!("hub stopped");
        }
        Ok(())
    }

    /// See `api::hub::hub_status`.
    pub(crate) fn hub_status(&self) -> HubStatusDto {
        let st = self.state.lock();
        let device_name = st
            .app
            .as_ref()
            .map(|a| a.settings.device_name.clone())
            .unwrap_or_default();
        match &st.hub {
            Some(slot) => HubStatusDto {
                running: true,
                port: slot.handle.local_port(),
                device_name,
                source_count: u32::try_from(slot.handle.sources().len()).unwrap_or(u32::MAX),
            },
            None => convert::stopped_hub_status(device_name),
        }
    }

    /// See `api::hub::hub_sources`.
    pub(crate) fn hub_sources(&self) -> Vec<SourceDto> {
        self.state
            .lock()
            .hub
            .as_ref()
            .map(|slot| {
                slot.handle
                    .sources()
                    .iter()
                    .map(convert::source_dto)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn with_hub<T>(&self, f: impl FnOnce(&HubHandle) -> Result<T>) -> Result<T> {
        let st = self.state.lock();
        let slot = st.hub.as_ref().ok_or(FfiError::HubNotRunning)?;
        f(&slot.handle)
    }

    /// See `api::hub::hub_set_gain`.
    pub(crate) fn hub_set_gain(&self, stream_id: u32, gain: f32) -> Result<()> {
        check_gain(gain)?;
        self.with_hub(|h| Ok(h.set_gain(stream_id, gain)?))
    }

    /// See `api::hub::hub_set_muted`.
    pub(crate) fn hub_set_muted(&self, stream_id: u32, muted: bool) -> Result<()> {
        self.with_hub(|h| Ok(h.set_muted(stream_id, muted)?))
    }

    /// See `api::hub::hub_set_priority`.
    pub(crate) fn hub_set_priority(&self, stream_id: u32, priority: bool) -> Result<()> {
        self.with_hub(|h| Ok(h.set_priority(stream_id, priority)?))
    }

    /// See `api::hub::hub_set_master_gain`.
    pub(crate) fn hub_set_master_gain(&self, gain: f32) -> Result<()> {
        check_gain(gain)?;
        self.with_hub(|h| {
            h.set_master_gain(gain);
            Ok(())
        })
    }

    /// See `api::hub::hub_start_pairing`.
    pub(crate) fn hub_start_pairing(&self) -> Result<PairingInfoDto> {
        self.with_hub(|h| Ok(convert::pairing_info_dto(&h.start_pairing())))
    }

    /// See `api::hub::hub_cancel_pairing`.
    pub(crate) fn hub_cancel_pairing(&self) -> Result<()> {
        self.with_hub(|h| {
            h.cancel_pairing();
            Ok(())
        })
    }

    /// See `api::hub::hub_events`.
    pub(crate) fn hub_events(&self, sink: Box<dyn EventSink<HubEventDto>>) {
        self.hub_sinks.add(sink);
    }

    // ---------------------------------------------------------------- sender

    /// See `api::sender::discover_hubs`.
    pub(crate) fn discover_hubs(&self, sink: Box<dyn EventSink<DiscoveryEventDto>>) -> Result<()> {
        let data_dir = self
            .state
            .lock()
            .app
            .as_ref()
            .map(|a| a.settings.data_dir.clone());
        let mut browser = {
            let _rt = self.runtime.enter();
            hfa_core::browse()?
        };
        let task = self.runtime.spawn(async move {
            while let Some(event) = browser.recv().await {
                // Loaded per event (a small file; events are rare) so that a hub paired
                // while discovery runs shows as trusted.
                let trust = data_dir.as_deref().and_then(|d| TrustStore::load(d).ok());
                let dto = convert::discovery_event_dto(&event, |id| {
                    trust.as_ref().is_some_and(|t| t.get(id).is_some())
                });
                if !sink.deliver(dto) {
                    break;
                }
            }
        });
        if let Some(previous) = self.state.lock().discovery.replace(task) {
            previous.abort();
        }
        Ok(())
    }

    /// See `api::sender::stop_discovery`.
    pub(crate) fn stop_discovery(&self) {
        if let Some(task) = self.state.lock().discovery.take() {
            task.abort();
        }
    }

    /// See `api::sender::sender_start`.
    pub(crate) fn sender_start(&self, req: SenderStartDto) -> Result<()> {
        let _ops = self.lifecycle.lock();
        let (settings, own_key, finished) = {
            let mut st = self.state.lock();
            let app = st.app.as_ref().ok_or(FfiError::NotInitialized)?;
            let (settings, own_key) = (app.settings.clone(), app.identity.public_key());
            let finished = match &st.sender {
                None => None,
                // A sender that gave up or stopped by itself is reaped, not "running".
                Some(slot) if is_finished(&slot.handle.status().state) => st.sender.take(),
                Some(_) => return Err(FfiError::SenderRunning),
            };
            (settings, own_key, finished)
        };
        let reaped = finished.is_some();
        if let Some(slot) = finished {
            if let Err(e) = self.reap_sender(slot) {
                tracing::warn!(error = %e, "stopping the finished sender failed");
            }
        }
        let result = self.start_sender(req, settings, &own_key);
        if result.is_err() && reaped {
            // The reaped sender's final state was the last status sent; there is no sender now.
            self.sender_sinks.broadcast(&convert::idle_sender_status());
        }
        result
    }

    /// `sender_start` once the slot is free (lifecycle lock held).
    fn start_sender(
        &self,
        req: SenderStartDto,
        settings: Settings,
        own_key: &[u8; 32],
    ) -> Result<()> {
        let trust = TrustStore::load(&settings.data_dir)?;
        let target = hub_target::resolve(
            &HubRequest {
                host: req.hub_host.clone(),
                port: req.hub_port,
                device_id: req.hub_device_id.clone(),
                key: req.hub_key.clone(),
            },
            settings.port,
            |id| trust.get(id).map(|p| p.public_key),
            own_key,
        )?;
        let (capture_target, feed_format) = convert::capture_target(&req.source)?;
        let feed_id = match (&capture_target, feed_format) {
            (hfa_capture::CaptureTarget::External { id }, Some(format)) => {
                feeds::register(*id, format);
                Some(*id)
            }
            _ => None,
        };
        let result = self.start_sender_engine(&req, &capture_target, settings, target);
        match result {
            Ok((handle, warning)) => {
                // Subscribe before anything else so that an early `Connected` (the only
                // source of the hub name) is not lost.
                let events = handle.events();
                // A capture fallback warning (macOS) is shown as the last non-fatal error.
                let meta = Arc::new(Mutex::new(SenderMeta::new(handle.status(), warning)));
                let initial = meta_dto(&meta.lock());
                // The slot is filled before the forwarder can relay anything, so a
                // subscription made meanwhile (`sender_events`) sees this sender.
                self.sender_sinks.broadcast_then(&initial, || {
                    let forwarder = self.runtime.spawn(forward_sender_events(
                        events,
                        Arc::clone(&meta),
                        Arc::clone(&self.sender_sinks),
                    ));
                    self.state.lock().sender = Some(SenderSlot {
                        handle,
                        forwarder,
                        meta,
                        feed_id,
                    });
                });
                tracing::info!(source = %capture_target, "sender started");
                Ok(())
            }
            Err(e) => {
                if let Some(id) = feed_id {
                    feeds::unregister(id);
                }
                Err(e)
            }
        }
    }

    /// Opens the capture (with `hfa_core`'s documented fallbacks) and starts the engine.
    /// Returns the handle and the capture's fallback warning, if any.
    fn start_sender_engine(
        &self,
        req: &SenderStartDto,
        capture_target: &hfa_capture::CaptureTarget,
        settings: Settings,
        target: hub_target::HubTarget,
    ) -> Result<(SenderHandle, Option<String>)> {
        let (capture, warning) = hfa_core::sender::open_capture(capture_target)?;
        let label = match req.label.trim() {
            "" => convert::default_label(&req.source),
            l => l.to_owned(),
        };
        let pairing_secret = req
            .pairing_secret
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let config = SenderConfig {
            hub: target.address,
            settings,
            capture,
            label,
            expected_hub_key: target.expected_key,
            pairing_secret,
        };
        let handle = block_on(&self.runtime, SenderEngine::start(config))??;
        Ok((handle, warning))
    }

    /// See `api::sender::sender_stop`.
    pub(crate) fn sender_stop(&self) -> Result<()> {
        let _ops = self.lifecycle.lock();
        let slot = self.state.lock().sender.take();
        if let Some(slot) = slot {
            let reaped = self.reap_sender(slot);
            // Sent after the forwarder has finished, so it is the last status.
            self.sender_sinks.broadcast(&convert::idle_sender_status());
            reaped?;
            tracing::info!("sender stopped");
        }
        Ok(())
    }

    /// Stops a sender taken out of its slot: stops the engine, lets the forwarder relay the
    /// remaining events and end, and unregisters the external feed.
    fn reap_sender(&self, slot: SenderSlot) -> Result<()> {
        let stopped = block_on(&self.runtime, slot.handle.stop());
        let drained = block_on(
            &self.runtime,
            finish_forwarder(slot.forwarder, FORWARDER_DRAIN),
        );
        if let Some(id) = slot.feed_id {
            feeds::unregister(id);
        }
        stopped.and(drained)
    }

    /// See `api::sender::sender_status`.
    pub(crate) fn sender_status(&self) -> SenderStatusDto {
        let st = self.state.lock();
        match &st.sender {
            Some(slot) => {
                let meta = slot.meta.lock();
                convert::sender_status_dto(
                    &slot.handle.status(),
                    meta.hub_name.as_deref(),
                    meta.last_error.as_deref(),
                )
            }
            None => convert::idle_sender_status(),
        }
    }

    /// See `api::sender::sender_events`.
    pub(crate) fn sender_events(&self, sink: Box<dyn EventSink<SenderStatusDto>>) {
        // Lock order: sinks, then state, then a sender's meta. The forwarder releases the
        // meta lock before it broadcasts, and nobody broadcasts while holding state or meta.
        self.sender_sinks
            .add_with_initial(sink, || self.sender_status());
    }
}

impl Drop for EngineManager {
    fn drop(&mut self) {
        // Only test instances are ever dropped (the global lives until exit). Abort the
        // background tasks; engines still running are torn down with the runtime.
        let st = self.state.get_mut();
        if let Some(task) = st.discovery.take() {
            task.abort();
        }
    }
}

fn app_info(app: &AppContext) -> AppInfo {
    AppInfo {
        device_id: app.identity.device_id.clone(),
        device_name: app.settings.device_name.clone(),
        platform: hfa_core::platform_name().to_owned(),
        version: hfa_core::APP_VERSION.to_owned(),
        capabilities: convert::capabilities_dto(hfa_capture::capabilities()),
    }
}

/// `true` once a sender will not stream any more by itself: it gave up
/// ([`SenderState::Failed`]) or was stopped. Such a sender no longer blocks `sender_start`.
fn is_finished(state: &SenderState) -> bool {
    matches!(state, SenderState::Failed(_) | SenderState::Stopped)
}

/// Waits up to `drain` for a forwarder to relay what is queued and end (its engine's
/// channel closes when the engine's tasks end), then aborts it and waits for that. After
/// this returns the forwarder sends nothing more.
async fn finish_forwarder(mut forwarder: JoinHandle<()>, drain: Duration) {
    if tokio::time::timeout(drain, &mut forwarder).await.is_err() {
        tracing::debug!("event forwarder still busy after the engine stopped; aborting it");
        forwarder.abort();
        // A cancellation `JoinError` is the expected outcome.
        let _ = forwarder.await;
    }
}

fn check_gain(gain: f32) -> Result<()> {
    if gain.is_finite() && (0.0..=hfa_audio::mixer::MAX_GAIN).contains(&gain) {
        Ok(())
    } else {
        Err(FfiError::InvalidArgument(format!(
            "gain {gain} outside 0..={}",
            hfa_audio::mixer::MAX_GAIN
        )))
    }
}

/// Relays hub engine events to the Dart subscriptions until the engine's channel closes.
async fn forward_hub_events(
    mut rx: broadcast::Receiver<HubEvent>,
    sinks: Arc<SinkSet<HubEventDto>>,
) {
    loop {
        match rx.recv().await {
            Ok(event) => sinks.broadcast(&convert::hub_event_dto(&event)),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(skipped = n, "hub event subscribers lagged");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Folds sender engine events into `meta` and relays the resulting status to the Dart
/// subscriptions until the engine's channel closes.
async fn forward_sender_events(
    mut rx: broadcast::Receiver<SenderEvent>,
    meta: Arc<Mutex<SenderMeta>>,
    sinks: Arc<SinkSet<SenderStatusDto>>,
) {
    loop {
        match rx.recv().await {
            Ok(event) => {
                let dto = {
                    let mut meta = meta.lock();
                    meta.apply(event);
                    meta_dto(&meta)
                };
                sinks.broadcast(&dto);
            }
            Err(broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(skipped = n, "sender event subscribers lagged");
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::sender::CaptureSourceDto;
    use hfa_core::SenderStatus;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Test sink recording what it receives; closes after `capacity` deliveries.
    #[derive(Clone)]
    struct Recorder<T> {
        got: Arc<Mutex<Vec<T>>>,
        capacity: usize,
        attempts: Arc<AtomicUsize>,
    }

    impl<T> Recorder<T> {
        fn new(capacity: usize) -> Self {
            Self {
                got: Arc::new(Mutex::new(Vec::new())),
                capacity,
                attempts: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl<T: Send> EventSink<T> for Recorder<T> {
        fn deliver(&self, value: T) -> bool {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            let mut got = self.got.lock();
            if got.len() >= self.capacity {
                return false;
            }
            got.push(value);
            true
        }
    }

    fn start_request() -> SenderStartDto {
        SenderStartDto {
            hub_host: "127.0.0.1".into(),
            hub_port: 0,
            hub_device_id: None,
            hub_key: None,
            pairing_secret: None,
            source: CaptureSourceDto::Tone { freq_hz: 440.0 },
            label: String::new(),
        }
    }

    #[test]
    fn calls_before_init_fail_cleanly() {
        let m = EngineManager::new().expect("manager");
        assert!(matches!(m.get_settings(), Err(FfiError::NotInitialized)));
        assert!(matches!(m.trusted_peers(), Err(FfiError::NotInitialized)));
        assert!(matches!(m.forget_peer("x"), Err(FfiError::NotInitialized)));
        assert!(matches!(m.hub_start(), Err(FfiError::NotInitialized)));
        assert!(matches!(
            m.sender_start(start_request()),
            Err(FfiError::NotInitialized)
        ));
        let dto = SettingsDto {
            device_name: "x".into(),
            port: 1,
            bitrate: 64_000,
            frame_ms: 10,
            fec: true,
            jitter_min_ms: 20,
            jitter_max_ms: 150,
            output_device: None,
        };
        assert!(matches!(
            m.update_settings(dto),
            Err(FfiError::NotInitialized)
        ));
        assert!(matches!(
            m.init_app(Path::new(""), None),
            Err(FfiError::InvalidArgument(_))
        ));
    }

    #[test]
    fn stopped_engines_report_idle_and_controls_fail() {
        let m = EngineManager::new().expect("manager");
        assert_eq!(m.hub_status(), convert::stopped_hub_status(String::new()));
        assert!(m.hub_sources().is_empty());
        assert!(matches!(
            m.hub_set_gain(1, 1.0),
            Err(FfiError::HubNotRunning)
        ));
        assert!(matches!(
            m.hub_set_muted(1, true),
            Err(FfiError::HubNotRunning)
        ));
        assert!(matches!(
            m.hub_set_priority(1, true),
            Err(FfiError::HubNotRunning)
        ));
        assert!(matches!(
            m.hub_set_master_gain(1.0),
            Err(FfiError::HubNotRunning)
        ));
        assert!(matches!(
            m.hub_start_pairing(),
            Err(FfiError::HubNotRunning)
        ));
        assert!(matches!(
            m.hub_cancel_pairing(),
            Err(FfiError::HubNotRunning)
        ));
        // Gain validation happens before the hub lookup.
        for bad in [-0.1, 4.1, f32::NAN, f32::INFINITY] {
            assert!(matches!(
                m.hub_set_gain(1, bad),
                Err(FfiError::InvalidArgument(_))
            ));
            assert!(matches!(
                m.hub_set_master_gain(bad),
                Err(FfiError::InvalidArgument(_))
            ));
        }
        // Stops are idempotent.
        m.hub_stop().expect("hub_stop");
        m.sender_stop().expect("sender_stop");
        m.stop_discovery();
        assert_eq!(m.sender_status().state, "idle");
    }

    #[test]
    fn sender_subscription_gets_the_current_status_first() {
        let m = EngineManager::new().expect("manager");
        let rec = Recorder::new(10);
        m.sender_events(Box::new(rec.clone()));
        assert_eq!(rec.got.lock().len(), 1);
        assert_eq!(rec.got.lock()[0].state, "idle");
        assert_eq!(m.sender_sinks.len(), 1);
        // A sink that is already closed is not kept.
        m.sender_events(Box::new(Recorder::<SenderStatusDto>::new(0)));
        assert_eq!(m.sender_sinks.len(), 1);
    }

    /// A broadcast racing a new subscription is delivered after the initial value.
    #[test]
    fn no_change_falls_between_the_snapshot_and_the_subscription() {
        let set = Arc::new(SinkSet::new());
        let rec = Recorder::new(10);
        let mut racer = None;
        set.add_with_initial(Box::new(rec.clone()), || {
            let set = Arc::clone(&set);
            // Blocks on the set's lock until the subscription is in place.
            racer = Some(std::thread::spawn(move || set.broadcast(&2)));
            std::thread::sleep(Duration::from_millis(50));
            1
        });
        racer.expect("spawned").join().expect("racer");
        assert_eq!(*rec.got.lock(), vec![1, 2]);
        // A closed sink is not kept.
        set.add_with_initial(Box::new(Recorder::<i32>::new(0)), || 3);
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn sink_set_drops_closed_subscriptions() {
        let set = SinkSet::new();
        let a = Recorder::new(100);
        let b = Recorder::new(2);
        set.add(Box::new(a.clone()));
        set.add(Box::new(b.clone()));
        for i in 0..5 {
            set.broadcast(&i);
        }
        assert_eq!(*a.got.lock(), vec![0, 1, 2, 3, 4]);
        assert_eq!(*b.got.lock(), vec![0, 1]);
        // b refused the third event and was dropped: no further attempts.
        assert_eq!(b.attempts.load(Ordering::SeqCst), 3);
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn hub_forwarder_relays_until_the_channel_closes() {
        let m = EngineManager::new().expect("manager");
        let rec = Recorder::new(10);
        m.hub_events(Box::new(rec.clone()));
        let (tx, rx) = broadcast::channel(16);
        let task = m
            .runtime
            .spawn(forward_hub_events(rx, Arc::clone(&m.hub_sinks)));
        tx.send(HubEvent::SourceRemoved { stream_id: 3 })
            .expect("send");
        tx.send(HubEvent::Error("boom".into())).expect("send");
        drop(tx);
        m.runtime.block_on(task).expect("forwarder ends");
        assert_eq!(
            *rec.got.lock(),
            vec![
                HubEventDto::SourceRemoved { stream_id: 3 },
                HubEventDto::Error {
                    message: "boom".into()
                }
            ]
        );
    }

    #[test]
    fn only_live_senders_block_a_new_start() {
        assert!(is_finished(&SenderState::Failed("pairing required".into())));
        assert!(is_finished(&SenderState::Stopped));
        for live in [
            SenderState::Connecting,
            SenderState::Pairing,
            SenderState::Streaming,
            SenderState::Reconnecting,
        ] {
            assert!(!is_finished(&live), "{live:?}");
        }
    }

    #[test]
    fn finished_forwarder_has_relayed_everything_queued() {
        let m = EngineManager::new().expect("manager");
        let rec = Recorder::new(1000);
        m.sender_sinks.add(Box::new(rec.clone()));
        let meta = Arc::new(Mutex::new(SenderMeta::new(SenderStatus::default(), None)));
        let (tx, rx) = broadcast::channel(512);
        let task = m.runtime.spawn(forward_sender_events(
            rx,
            Arc::clone(&meta),
            Arc::clone(&m.sender_sinks),
        ));
        for _ in 0..300 {
            tx.send(SenderEvent::StateChanged(SenderState::Streaming))
                .expect("send");
        }
        tx.send(SenderEvent::StateChanged(SenderState::Stopped))
            .expect("send");
        drop(tx);
        block_on(&m.runtime, finish_forwarder(task, Duration::from_secs(5))).expect("join");
        // What sender_stop sends next is therefore the last status a subscriber sees.
        m.sender_sinks.broadcast(&convert::idle_sender_status());
        let got = rec.got.lock();
        assert_eq!(got.len(), 302);
        assert_eq!(got[300].state, "stopped");
        assert_eq!(got[301].state, "idle");
    }

    #[test]
    fn stuck_forwarder_is_aborted_and_silenced() {
        let m = EngineManager::new().expect("manager");
        let rec = Recorder::new(1000);
        m.hub_events(Box::new(rec.clone()));
        let (tx, rx) = broadcast::channel(16);
        let task = m
            .runtime
            .spawn(forward_hub_events(rx, Arc::clone(&m.hub_sinks)));
        tx.send(HubEvent::SourceRemoved { stream_id: 1 })
            .expect("send");
        // The channel stays open (an engine task outlived `stop`): the drain times out.
        let started = std::time::Instant::now();
        block_on(
            &m.runtime,
            finish_forwarder(task, Duration::from_millis(50)),
        )
        .expect("join");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(
            *rec.got.lock(),
            vec![HubEventDto::SourceRemoved { stream_id: 1 }]
        );
        // The aborted forwarder has dropped its receiver: nothing is relayed any more.
        assert!(tx.send(HubEvent::SourceRemoved { stream_id: 2 }).is_err());
        assert_eq!(rec.got.lock().len(), 1);
    }

    #[test]
    fn sender_forwarder_folds_events() {
        let m = EngineManager::new().expect("manager");
        let rec = Recorder::new(10);
        m.sender_sinks.add(Box::new(rec.clone()));
        let meta = Arc::new(Mutex::new(SenderMeta::new(SenderStatus::default(), None)));
        let (tx, rx) = broadcast::channel(16);
        let task = m.runtime.spawn(forward_sender_events(
            rx,
            Arc::clone(&meta),
            Arc::clone(&m.sender_sinks),
        ));
        tx.send(SenderEvent::Connected {
            device_id: "id".into(),
            name: "Desk".into(),
        })
        .expect("send");
        tx.send(SenderEvent::StateChanged(SenderState::Streaming))
            .expect("send");
        tx.send(SenderEvent::Status(SenderStatus {
            state: SenderState::Streaming,
            bitrate: 128_000,
            loss_pct: 0.5,
            rtt_ms: 3.0,
            level_db: -12.0,
        }))
        .expect("send");
        tx.send(SenderEvent::Error("hiccup".into())).expect("send");
        tx.send(SenderEvent::StateChanged(SenderState::Failed(
            "hub gone".into(),
        )))
        .expect("send");
        drop(tx);
        m.runtime.block_on(task).expect("forwarder ends");
        let got = rec.got.lock();
        let states: Vec<&str> = got.iter().map(|s| s.state.as_str()).collect();
        assert_eq!(
            states,
            [
                "connecting",
                "streaming",
                "streaming",
                "streaming",
                "failed"
            ]
        );
        assert_eq!(got[0].hub_name.as_deref(), Some("Desk"));
        assert_eq!(got[2].bitrate, 128_000);
        assert_eq!(got[3].error.as_deref(), Some("hiccup"));
        assert_eq!(got[4].error.as_deref(), Some("hub gone"));
        assert_eq!(got[4].hub_name.as_deref(), Some("Desk"));
    }
}
