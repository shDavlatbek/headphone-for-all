//! The sender engine: capture → (convert to 48 kHz stereo) → Opus → seal → UDP, controlled
//! over a [`crate::ControlChannel`].
//!
//! # Structure
//!
//! - **Encoder thread** (`hfa-encoder`, soft real-time, see `sender_encoder.rs`): owns the
//!   capture for the whole life of the sender, converts to 48 kHz stereo, cuts exact
//!   `frame_ms` frames, detects silence (DTX keep-alives), encodes with Opus, builds the
//!   [media payload container](crate::payload) (with the previous frame as redundancy when
//!   enabled) and sends through the current [`MediaSender`].
//! - **Control task** (tokio): resolves the hub, connects ([`ControlChannel::connect`], with
//!   pairing when needed), announces a stream (`StreamStart` with a fresh `stream_id` and
//!   media key), waits for `StreamAccepted{udp_port}` ([`STREAM_ACCEPT_TIMEOUT`]), hands the
//!   stream to the encoder thread, then pings the hub every [`PING_INTERVAL`] (RTT), adapts
//!   to the hub's `Stats` (redundancy, expected loss, bitrate; see `sender_adapt.rs`),
//!   reports the hub's `SetVolume`/`SetMute`/`SetPriority` as [`SenderEvent::HubControl`] and
//!   publishes [`SenderStatus`] once per second. The controls the hub announces before
//!   `StreamAccepted` describe the new stream and replace those of any earlier stream (one
//!   `HubControl` event if that changes what was shown).
//! - **Reconnects:** any control or media failure (including a hub that stays silent for
//!   [`HUB_TIMEOUT`] or says `Bye`) ends the session; the control task waits with exponential
//!   backoff ([`INITIAL_BACKOFF`] doubling up to [`MAX_BACKOFF`], back to the start after a
//!   session that streamed) and starts a **new** stream (new `stream_id` and key). Pairing
//!   and key errors ([`crate::CoreError::PairingRequired`], [`crate::CoreError::PairingFailed`],
//!   [`crate::CoreError::KeyMismatch`]) and [`crate::CoreError::SelfConnection`] (the hub is
//!   this very device) are final: the state becomes [`SenderState::Failed`].
//!   After the first connection the hub's key is pinned for every reconnect, and a pairing
//!   secret is used at most once.
//! - **Capture failure:** a capture that fails for good ([`CaptureSource::error`], polled by
//!   the encoder thread) stops the stream (`StreamStop`) and the sender with
//!   [`SenderState::Failed`]`("the audio capture stopped: <reason>")`, instead of passing for
//!   silence (DTX keep-alives while showing `Streaming`).
//! - [`SenderHandle::stop`] sends `StreamStop` + `Bye`, stops the encoder thread and the
//!   capture, and waits for everything.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use hfa_audio::AudioFormat;
use hfa_capture::{CaptureError, CaptureSource, CaptureTarget};
use hfa_proto::control::{Body, Ping, Pong, StreamStart, StreamStop};
use hfa_proto::ControlMessage;
use serde::{Deserialize, Serialize};
use tokio::net::UdpSocket;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;

use crate::config::Settings;
use crate::control::{ControlChannel, PeerInfo};
use crate::identity::{Identity, TrustStore};
use crate::media::MediaSender;
use crate::sender_adapt::Adapter;
use crate::sender_encoder::{EncoderCommand, EncoderEvent, EncoderParams, EncoderShared};
use crate::{CoreError, Result};

/// First reconnect delay.
pub const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
/// Longest reconnect delay.
pub const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// How long the sender waits for `StreamAccepted` after `StreamStart`.
pub const STREAM_ACCEPT_TIMEOUT: Duration = Duration::from_secs(10);
/// Interval between two `Ping`s (RTT measurement and liveness).
pub const PING_INTERVAL: Duration = Duration::from_secs(1);
/// A hub that sent nothing (no `Pong`, no `Stats`) for this long is considered gone.
pub const HUB_TIMEOUT: Duration = Duration::from_secs(6);
/// How long a [`HubAddress::Discover`] lookup browses mDNS before giving up.
pub const DISCOVER_TIMEOUT: Duration = Duration::from_secs(5);
/// Capacity of the capture ring, in ms of capture audio.
const CAPTURE_RING_MS: usize = 500;
/// Capacity of the event channel.
const EVENT_CAPACITY: usize = 64;
/// Interval of [`SenderEvent::Status`] events.
const STATUS_INTERVAL: Duration = Duration::from_secs(1);
/// The hub's controls for our stream: gain, muted, priority.
type HubControls = (f32, bool, bool);
/// Controls of a stream the hub announced nothing for.
const DEFAULT_HUB_CONTROLS: HubControls = (1.0, false, false);
/// Every this many pings the trust store is re-read from disk (5 s).
const TRUST_RELOAD_PINGS: u64 = 5;
/// Consecutive `Stats` saying the hub receives nothing ([`crate::hub::NO_MEDIA_LOSS_PCT`])
/// before the sender reports it as an error.
const NO_MEDIA_REPORTS: u32 = 3;

/// How to reach the hub.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum HubAddress {
    /// A host name or IP address and TCP port (`--to host[:port]`, pairing URI).
    Direct {
        /// Host name or IP address.
        host: String,
        /// TCP control port.
        port: u16,
    },
    /// Resolve through mDNS by device id or display name (`--hub <name-or-id>`).
    ///
    /// mDNS records are unauthenticated, so: when several hubs match a name, trusted hubs
    /// (in the sender's [`crate::TrustStore`]) are preferred; when `name_or_id` is a device
    /// id, the engine checks after the handshake that the fingerprint of the hub's Noise
    /// static key equals it and fails with [`crate::CoreError::KeyMismatch`] otherwise.
    Discover {
        /// Device id (exact) or display name (case-insensitive).
        name_or_id: String,
    },
}

/// Everything needed to start a sender.
pub struct SenderConfig {
    /// Hub to stream to.
    pub hub: HubAddress,
    /// Settings (bitrate, frame size, FEC, data dir for identity/trust store...).
    pub settings: Settings,
    /// The capture source (not yet started; the engine starts and stops it).
    pub capture: Box<dyn CaptureSource>,
    /// Label shown on the hub (e.g. "System audio").
    pub label: String,
    /// The hub's Noise static public key, if known in advance (`PairingUri::hub_id`, or a
    /// trusted peer chosen by the caller). Passed to [`crate::ControlChannel::connect`], which
    /// fails with [`crate::CoreError::KeyMismatch`] if the hub presents another key.
    pub expected_hub_key: Option<[u8; 32]>,
    /// PIN or token, needed whenever pairing is: this device does not trust the hub yet, or
    /// the hub does not trust this device. Without it such a connection fails with
    /// [`crate::CoreError::PairingRequired`]; the sender never streams to an untrusted hub.
    pub pairing_secret: Option<String>,
}

impl fmt::Debug for SenderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SenderConfig")
            .field("hub", &self.hub)
            .field("settings", &self.settings)
            .field("capture", &self.capture.describe())
            .field("label", &self.label)
            .field("expected_hub_key", &self.expected_hub_key)
            .field(
                "pairing_secret",
                &self.pairing_secret.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// Sender lifecycle state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SenderState {
    /// Resolving/connecting to the hub.
    Connecting,
    /// Pairing with the hub.
    Pairing,
    /// Streaming audio.
    Streaming,
    /// Connection lost; waiting to reconnect.
    Reconnecting,
    /// Stopped by the user.
    Stopped,
    /// Gave up (e.g. pairing failed, key mismatch).
    Failed(String),
}

/// Snapshot of the sender's state and metrics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SenderStatus {
    /// Lifecycle state.
    pub state: SenderState,
    /// Current Opus bitrate in bits per second.
    pub bitrate: u32,
    /// Loss reported by the hub, in percent.
    pub loss_pct: f32,
    /// Control-channel round-trip time in ms.
    pub rtt_ms: f32,
    /// Current capture level in dBFS.
    pub level_db: f32,
}

impl Default for SenderStatus {
    fn default() -> Self {
        Self {
            state: SenderState::Connecting,
            bitrate: 0,
            loss_pct: 0.0,
            rtt_ms: 0.0,
            level_db: hfa_audio::meter::SILENCE_DB,
        }
    }
}

/// Notifications from a running sender.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SenderEvent {
    /// The lifecycle state changed.
    StateChanged(SenderState),
    /// Connected to (and authenticated) the hub.
    Connected {
        /// Hub device id.
        device_id: String,
        /// Hub name.
        name: String,
    },
    /// Pairing with the hub succeeded (it is now trusted).
    Paired {
        /// Hub device id.
        device_id: String,
        /// Hub name.
        name: String,
    },
    /// The hub changed this stream's volume/mute/priority (informational).
    HubControl {
        /// Linear gain on the hub.
        gain: f32,
        /// Muted on the hub.
        muted: bool,
        /// Priority on the hub.
        priority: bool,
    },
    /// Periodic status (about once per second).
    Status(SenderStatus),
    /// A non-fatal error.
    Error(String),
}

/// Opens a capture source for a sender, with the documented fallbacks, and returns it with an
/// optional warning for the user.
///
/// On macOS, [`CaptureTarget::SystemMixExcludingSelf`] fails with
/// [`CaptureError::Backend`] when this process has no Core Audio process object; the sender
/// then captures the whole system mix instead ([`CaptureTarget::SystemMix`]) and returns a
/// warning saying so (the caller shows it: the engine's events cannot be subscribed to before
/// the engine exists). Every other target is opened as is.
///
/// # Errors
/// [`CoreError::Capture`] from [`hfa_capture::open_capture`].
pub fn open_capture(target: &CaptureTarget) -> Result<(Box<dyn CaptureSource>, Option<String>)> {
    match hfa_capture::open_capture(target) {
        Ok(capture) => Ok((capture, None)),
        Err(CaptureError::Backend(reason))
            if cfg!(target_os = "macos") && *target == CaptureTarget::SystemMixExcludingSelf =>
        {
            let capture = hfa_capture::open_capture(&CaptureTarget::SystemMix)?;
            let warning = format!(
                "cannot exclude this app from the capture ({reason}); capturing the whole system \
                 mix instead"
            );
            tracing::warn!("{warning}");
            Ok((capture, Some(warning)))
        }
        Err(e) => Err(e.into()),
    }
}

/// Entry point of the sender engine.
pub struct SenderEngine;

impl SenderEngine {
    /// Starts the sender on the current tokio runtime: loads the identity and trust store from
    /// `settings.data_dir`, starts the capture into a ring and the encoder thread, and spawns
    /// the control task. Returns immediately (before the connection is up); connection
    /// progress is reported through [`SenderHandle::status`] and [`SenderHandle::events`].
    ///
    /// # Errors
    /// Invalid settings or hub address ([`CoreError::Config`]), identity/trust store I/O
    /// errors, a capture that cannot start ([`CoreError::Capture`]) or thread creation
    /// failures.
    pub async fn start(config: SenderConfig) -> Result<SenderHandle> {
        let SenderConfig {
            hub,
            settings,
            capture,
            label,
            expected_hub_key,
            pairing_secret,
        } = config;
        settings.validate()?;
        match &hub {
            HubAddress::Direct { host, port } if host.trim().is_empty() || *port == 0 => {
                return Err(CoreError::Config(format!(
                    "invalid hub address {host:?} port {port}"
                )));
            }
            HubAddress::Discover { name_or_id } if name_or_id.trim().is_empty() => {
                return Err(CoreError::Config("empty hub name".into()));
            }
            _ => {}
        }
        let format = capture.format();
        if format.sample_rate == 0 || format.channels == 0 {
            return Err(CoreError::Config(format!(
                "capture format {} Hz / {} channels is invalid",
                format.sample_rate, format.channels
            )));
        }
        let (identity, trust) = load_identity(&settings).await?;

        let ring_samples =
            format.sample_rate as usize * usize::from(format.channels) * CAPTURE_RING_MS / 1000;
        let (sink, source) = hfa_capture::pcm_ring_with_channels(ring_samples, format.channels);
        let mut capture = capture;
        let (capture, started) = tokio::task::spawn_blocking(move || {
            let result = capture.start(sink);
            (capture, result)
        })
        .await
        .map_err(|e| CoreError::Io(format!("capture start task failed: {e}")))?;
        started?;
        tracing::info!(
            capture = %capture.describe(),
            rate = format.sample_rate,
            channels = format.channels,
            hub = ?hub,
            "sender started"
        );

        let shared = Arc::new(EncoderShared::new());
        let (encoder_tx, encoder_rx) = std::sync::mpsc::channel();
        let (encoder_events_tx, encoder_events) = mpsc::unbounded_channel();
        let thread = crate::sender_encoder::spawn(
            capture,
            source,
            format,
            EncoderParams {
                frame_ms: settings.frame_ms,
                fec: settings.fec,
            },
            Arc::clone(&shared),
            encoder_rx,
            encoder_events_tx,
        )?;

        let adapter = Adapter::new(settings.bitrate, if settings.fec { 5 } else { 0 });
        let (status_tx, status_rx) = watch::channel(SenderStatus {
            state: SenderState::Connecting,
            bitrate: settings.bitrate,
            ..SenderStatus::default()
        });
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let (stop_tx, stop_rx) = watch::channel(false);
        let control = Control {
            hub,
            settings,
            label,
            identity,
            trust,
            expected_hub_key,
            secret: pairing_secret.map(zeroize::Zeroizing::new),
            pinned: None,
            adapter,
            status: status_tx,
            events: events.clone(),
            encoder: encoder_tx,
            encoder_events,
            shared: Arc::clone(&shared),
            stop: stop_rx,
            epoch: Instant::now(),
            hub_control: DEFAULT_HUB_CONTROLS,
            no_media_reports: 0,
        };
        let task = tokio::spawn(control.run(thread));
        Ok(SenderHandle {
            events,
            status: status_rx,
            shared,
            stop: stop_tx,
            task,
        })
    }
}

/// Loads (or creates) the identity and the trust store off the async workers.
pub(crate) async fn load_identity(settings: &Settings) -> Result<(Identity, TrustStore)> {
    let dir = settings.data_dir.clone();
    let name = settings.device_name.clone();
    tokio::task::spawn_blocking(move || {
        let identity = Identity::load_or_create(&dir, &name)?;
        let trust = TrustStore::load(&dir)?;
        Ok((identity, trust))
    })
    .await
    .map_err(|e| CoreError::Io(format!("identity loading task failed: {e}")))?
}

/// Resolves once `stop` is `true` or its sender is gone.
pub(crate) async fn wait_stop(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|stopped| *stopped).await;
}

/// Handle to a running sender. `Send + Sync`. Dropping it stops the sender in the background.
pub struct SenderHandle {
    events: broadcast::Sender<SenderEvent>,
    status: watch::Receiver<SenderStatus>,
    shared: Arc<EncoderShared>,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl fmt::Debug for SenderHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SenderHandle")
            .field("status", &*self.status.borrow())
            .finish_non_exhaustive()
    }
}

impl SenderHandle {
    /// Current status snapshot (the level is live).
    pub fn status(&self) -> SenderStatus {
        let mut status = self.status.borrow().clone();
        status.level_db = match status.state {
            SenderState::Stopped | SenderState::Failed(_) => hfa_audio::meter::SILENCE_DB,
            _ => self.shared.level_db(),
        };
        status
    }

    /// Subscribes to events.
    pub fn events(&self) -> broadcast::Receiver<SenderEvent> {
        self.events.subscribe()
    }

    /// Audio datagrams and DTX keep-alives sent so far (all streams), for diagnostics.
    pub fn packets_sent(&self) -> (u64, u64) {
        (
            self.shared.packets_sent.load(Ordering::Relaxed),
            self.shared.keepalives_sent.load(Ordering::Relaxed),
        )
    }

    /// Stops streaming (sends `StreamStop` + `Bye`), stops the capture and waits for all tasks.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        if let Err(e) = self.task.await {
            tracing::warn!(error = %e, "sender control task failed");
        }
    }
}

/// How a session (one connection) ended.
enum SessionEnd {
    /// [`SenderHandle::stop`] was called.
    Stopped,
    /// Connection lost or refused; try again after a backoff.
    Retry {
        reason: String,
        /// The session got as far as streaming (resets the backoff).
        streamed: bool,
    },
    /// Give up (pairing or key errors).
    Fatal(CoreError),
    /// The capture failed for good: give up with this reason.
    CaptureFailed(String),
}

fn end_with(error: CoreError, streamed: bool) -> SessionEnd {
    match error {
        CoreError::PairingRequired
        | CoreError::PairingFailed(_)
        | CoreError::KeyMismatch(_)
        | CoreError::SelfConnection => SessionEnd::Fatal(error),
        other => SessionEnd::Retry {
            reason: other.to_string(),
            streamed,
        },
    }
}

/// The control task's state.
struct Control {
    hub: HubAddress,
    settings: Settings,
    label: String,
    identity: Identity,
    trust: TrustStore,
    expected_hub_key: Option<[u8; 32]>,
    /// Pairing secret; cleared after a successful pairing (one-time secrets).
    secret: Option<zeroize::Zeroizing<String>>,
    /// The hub's key after the first connection (every reconnect must reach the same hub).
    pinned: Option<[u8; 32]>,
    adapter: Adapter,
    status: watch::Sender<SenderStatus>,
    events: broadcast::Sender<SenderEvent>,
    encoder: std::sync::mpsc::Sender<EncoderCommand>,
    encoder_events: mpsc::UnboundedReceiver<EncoderEvent>,
    shared: Arc<EncoderShared>,
    stop: watch::Receiver<bool>,
    epoch: Instant,
    /// Gain, mute and priority the hub applies to our stream.
    hub_control: HubControls,
    /// Consecutive `Stats` of the current stream saying the hub receives nothing.
    no_media_reports: u32,
}

impl Control {
    async fn run(mut self, encoder_thread: std::thread::JoinHandle<()>) {
        let mut backoff = INITIAL_BACKOFF;
        let final_state = loop {
            if *self.stop.borrow() {
                break SenderState::Stopped;
            }
            self.set_state(SenderState::Connecting);
            match self.session().await {
                SessionEnd::Stopped => break SenderState::Stopped,
                SessionEnd::Fatal(error) => {
                    let message = error.to_string();
                    tracing::warn!(error = %message, "sender failed");
                    self.emit(SenderEvent::Error(message.clone()));
                    break SenderState::Failed(message);
                }
                SessionEnd::CaptureFailed(reason) => break self.capture_failed(reason),
                SessionEnd::Retry { reason, streamed } => {
                    self.command(EncoderCommand::Clear);
                    if streamed {
                        backoff = INITIAL_BACKOFF;
                    }
                    tracing::info!(%reason, retry_in = ?backoff, "hub connection lost");
                    self.emit(SenderEvent::Error(format!(
                        "{reason}; reconnecting in {} s",
                        backoff.as_secs()
                    )));
                    self.set_state(SenderState::Reconnecting);
                    if let Some(end) = self.wait_backoff(backoff).await {
                        break end;
                    }
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                }
            }
        };
        self.command(EncoderCommand::Clear);
        self.shared.stop.store(true, Ordering::Release);
        if tokio::task::spawn_blocking(move || encoder_thread.join())
            .await
            .is_err()
        {
            tracing::warn!("encoder thread join failed");
        }
        self.set_state(final_state);
        self.publish_status();
    }

    fn emit(&self, event: SenderEvent) {
        let _ = self.events.send(event);
    }

    /// Waits `backoff` before a reconnect; returns the final state if the sender was stopped
    /// or its capture failed for good meanwhile.
    async fn wait_backoff(&mut self, backoff: Duration) -> Option<SenderState> {
        let mut stop = self.stop.clone();
        let sleep = tokio::time::sleep(backoff);
        tokio::pin!(sleep);
        loop {
            tokio::select! {
                _ = &mut sleep => return None,
                _ = wait_stop(&mut stop) => return Some(SenderState::Stopped),
                event = self.encoder_events.recv() => match event {
                    Some(EncoderEvent::CaptureFailed(reason)) => {
                        return Some(self.capture_failed(reason));
                    }
                    Some(EncoderEvent::Warning(message)) => {
                        tracing::warn!(%message, "encoder");
                        self.emit(SenderEvent::Error(message));
                    }
                    Some(EncoderEvent::StreamFailed { .. }) => {}
                    None => return Some(SenderState::Failed(CoreError::Closed.to_string())),
                },
            }
        }
    }

    /// The capture died: report it and give up (a final state).
    fn capture_failed(&self, reason: String) -> SenderState {
        let message = format!("the audio capture stopped: {reason}");
        tracing::warn!(%message, "sender failed");
        self.emit(SenderEvent::Error(message.clone()));
        SenderState::Failed(message)
    }

    fn command(&self, command: EncoderCommand) {
        // Fails only once the encoder thread is gone; nothing left to control then.
        let _ = self.encoder.send(command);
    }

    fn set_state(&self, state: SenderState) {
        let changed = self.status.send_if_modified(|s| {
            if s.state == state {
                false
            } else {
                s.state = state.clone();
                true
            }
        });
        if changed {
            self.emit(SenderEvent::StateChanged(state));
        }
    }

    fn publish_status(&self) {
        let level = self.shared.level_db();
        self.status.send_modify(|s| s.level_db = level);
        let status = self.status.borrow().clone();
        self.emit(SenderEvent::Status(status));
    }

    fn now_us(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_micros()).unwrap_or(u64::MAX)
    }

    /// One connection: resolve, connect (pairing), stream until something ends it.
    async fn session(&mut self) -> SessionEnd {
        let mut stop = self.stop.clone();
        let connected = tokio::select! {
            r = self.connect_phase() => r,
            _ = wait_stop(&mut stop) => return SessionEnd::Stopped,
        };
        match connected {
            Ok((ch, peer)) => {
                self.on_connected(&peer);
                self.stream_phase(ch).await
            }
            Err(e) => end_with(e, false),
        }
    }

    async fn connect_phase(&mut self) -> Result<(ControlChannel, PeerInfo)> {
        let (addrs, discovered_key, required_id) = self.resolve().await?;
        let key = self.pinned.or(self.expected_hub_key).or(discovered_key);
        if self.secret.is_some() && !key.is_some_and(|k| self.trust.is_trusted(&k)) {
            self.set_state(SenderState::Pairing);
        }
        // Every address in turn (IPv4 first), until one answers: a host may have an address
        // the hub cannot be reached on (another family, another network).
        let mut connected = Err(CoreError::HubNotFound("no address".into()));
        for addr in addrs {
            let secret = self.secret.as_ref().map(|s| s.as_str().to_owned());
            connected =
                ControlChannel::connect(addr, &self.identity, &self.trust, key, secret).await;
            match &connected {
                Err(CoreError::Io(e)) | Err(CoreError::Timeout(e)) => {
                    tracing::debug!(%addr, error = %e, "hub address unreachable");
                }
                _ => break,
            }
        }
        let (ch, peer) = connected?;
        if let Some(id) = required_id {
            if peer.device_id != id {
                let _ = ch.close("unexpected hub").await;
                return Err(CoreError::KeyMismatch(peer.device_id));
            }
        }
        Ok((ch, peer))
    }

    fn on_connected(&mut self, peer: &PeerInfo) {
        tracing::info!(hub = %peer.device_id, name = %peer.name, addr = %peer.addr, "connected to hub");
        self.pinned = Some(peer.public_key);
        self.emit(SenderEvent::Connected {
            device_id: peer.device_id.clone(),
            name: peer.name.clone(),
        });
        if peer.newly_paired {
            // One-time secret: never offer it again.
            self.secret = None;
            self.emit(SenderEvent::Paired {
                device_id: peer.device_id.clone(),
                name: peer.name.clone(),
            });
        }
    }

    /// Hub socket addresses (IPv4 first), the hub key known from the trust store
    /// (discovery), and the device id the hub must have (discovery by id / pinned hub).
    async fn resolve(&self) -> Result<(Vec<SocketAddr>, Option<[u8; 32]>, Option<String>)> {
        match &self.hub {
            HubAddress::Direct { host, port } => {
                let host = host.trim().trim_start_matches('[').trim_end_matches(']');
                let mut addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, *port))
                    .await
                    .map_err(|e| CoreError::HubNotFound(format!("{host}: {e}")))?
                    .collect();
                if addrs.is_empty() {
                    return Err(CoreError::HubNotFound(host.to_owned()));
                }
                addrs.sort_by_key(SocketAddr::is_ipv6);
                addrs.dedup();
                Ok((addrs, None, None))
            }
            HubAddress::Discover { name_or_id } => {
                let target = match self.pinned {
                    Some(key) => hfa_proto::fingerprint(&key),
                    None => name_or_id.trim().to_owned(),
                };
                discover(&target, &self.trust).await
            }
        }
    }

    /// `StreamStart` → `StreamAccepted`, then the media sender aimed at the hub's UDP port,
    /// and the hub's controls for the new stream (announced before `StreamAccepted`;
    /// defaults if it announced none).
    async fn start_stream(
        &mut self,
        ch: &mut ControlChannel,
    ) -> Result<(MediaSender, HubControls)> {
        let hub_ip = ch.peer_addr().ip();
        let bind: SocketAddr = match hub_ip {
            IpAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
            IpAddr::V6(_) => (Ipv6Addr::UNSPECIFIED, 0).into(),
        };
        let socket = Arc::new(UdpSocket::bind(bind).await?);
        let stream_id = loop {
            let id = rand::random::<u32>();
            if id != 0 {
                break id;
            }
        };
        let mut media = MediaSender::new(socket, ch.peer_addr(), stream_id);
        let bitrate = self.adapter.current().bitrate;
        ch.send(&ControlMessage::new(Body::StreamStart(StreamStart {
            stream_id,
            sample_rate: AudioFormat::INTERNAL.sample_rate,
            channels: u32::from(AudioFormat::INTERNAL.channels),
            frame_ms: self.settings.frame_ms,
            bitrate,
            label: self.label.clone(),
            media_key: media.key().as_bytes().to_vec(),
        })))
        .await?;
        let deadline = tokio::time::Instant::now() + STREAM_ACCEPT_TIMEOUT;
        let mut controls = DEFAULT_HUB_CONTROLS;
        loop {
            let msg = tokio::time::timeout_at(deadline, ch.recv())
                .await
                .map_err(|_| CoreError::Timeout("waiting for StreamAccepted".into()))??;
            match msg.body {
                Some(Body::StreamAccepted(a)) if a.stream_id == stream_id => {
                    let port = u16::try_from(a.udp_port)
                        .ok()
                        .filter(|p| *p != 0)
                        .ok_or_else(|| {
                            CoreError::Protocol(format!("invalid media port {}", a.udp_port))
                        })?;
                    media.set_dest(SocketAddr::new(hub_ip, port));
                    return Ok((media, controls));
                }
                Some(Body::SetVolume(v)) if v.stream_id == stream_id => controls.0 = v.gain,
                Some(Body::SetMute(m)) if m.stream_id == stream_id => controls.1 = m.muted,
                Some(Body::SetPriority(p)) if p.stream_id == stream_id => controls.2 = p.priority,
                Some(Body::StreamRejected(r)) if r.stream_id == stream_id => {
                    return Err(CoreError::Rejected(r.reason));
                }
                Some(Body::Bye(b)) => {
                    return Err(CoreError::Protocol(format!(
                        "hub closed the connection: {}",
                        b.reason
                    )));
                }
                Some(Body::Ping(p)) => {
                    ch.send(&ControlMessage::new(Body::Pong(Pong {
                        nonce: p.nonce,
                        t_us: p.t_us,
                    })))
                    .await?;
                }
                _ => {}
            }
        }
    }

    async fn stream_phase(&mut self, mut ch: ControlChannel) -> SessionEnd {
        let mut stop = self.stop.clone();
        let started = tokio::select! {
            r = self.start_stream(&mut ch) => Some(r),
            _ = wait_stop(&mut stop) => None,
        };
        let media = match started {
            None => {
                let _ = ch.close("sender stopped").await;
                return SessionEnd::Stopped;
            }
            Some(Err(e)) => {
                let _ = ch.close(&e.to_string()).await;
                return end_with(e, false);
            }
            Some(Ok(started)) => started,
        };
        let (media, controls) = media;
        // The hub's view of this new stream replaces whatever an earlier stream showed.
        if controls != self.hub_control {
            self.hub_control = controls;
            self.emit_hub_control();
        }
        let stream_id = media.stream_id();
        let media_port = media.dest().port();
        self.no_media_reports = 0;
        tracing::info!(stream_id, dest = %media.dest(), "streaming");
        self.command(EncoderCommand::Stream {
            media,
            adaptation: self.adapter.current(),
        });
        self.status.send_modify(|s| {
            s.bitrate = self.adapter.current().bitrate;
            s.loss_pct = 0.0;
        });
        self.set_state(SenderState::Streaming);
        self.publish_status();

        let mut ping = tokio::time::interval(PING_INTERVAL);
        ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut status_tick = tokio::time::interval(STATUS_INTERVAL);
        status_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut last_rx = Instant::now();
        let mut nonce = 0u64;
        loop {
            tokio::select! {
                msg = ch.recv() => {
                    let msg = match msg {
                        Ok(m) => m,
                        Err(e) => return end_with(e, true),
                    };
                    last_rx = Instant::now();
                    if let Some(end) = self.on_message(&mut ch, msg, stream_id, media_port).await {
                        self.command(EncoderCommand::Clear);
                        let _ = ch.close("stream ended").await;
                        return end;
                    }
                }
                _ = ping.tick() => {
                    if self.pinned.is_some_and(|k| !self.trust.is_trusted(&k)) {
                        // The hub was forgotten on this device while streaming (trust is
                        // otherwise only checked at the handshake).
                        self.command(EncoderCommand::Clear);
                        let _ = ch.close("hub no longer trusted").await;
                        tracing::info!("the hub was removed from the trusted devices; stopping");
                        return SessionEnd::Fatal(CoreError::PairingRequired);
                    }
                    if nonce % TRUST_RELOAD_PINGS == TRUST_RELOAD_PINGS - 1 {
                        crate::hub::reload_trust(&self.trust).await;
                    }
                    if last_rx.elapsed() > HUB_TIMEOUT {
                        self.command(EncoderCommand::Clear);
                        let _ = ch.close("no answer").await;
                        return SessionEnd::Retry {
                            reason: format!("the hub did not answer for {} s", HUB_TIMEOUT.as_secs()),
                            streamed: true,
                        };
                    }
                    nonce = nonce.wrapping_add(1);
                    let msg = ControlMessage::new(Body::Ping(Ping { nonce, t_us: self.now_us() }));
                    if let Err(e) = ch.send(&msg).await {
                        return end_with(e, true);
                    }
                }
                _ = status_tick.tick() => self.publish_status(),
                event = self.encoder_events.recv() => match event {
                    Some(EncoderEvent::StreamFailed { stream_id: id, error }) if id == stream_id => {
                        let _ = ch.close("media failed").await;
                        return end_with(error, true);
                    }
                    Some(EncoderEvent::StreamFailed { .. }) => {}
                    Some(EncoderEvent::Warning(message)) => {
                        tracing::warn!(%message, "encoder");
                        self.emit(SenderEvent::Error(message));
                    }
                    Some(EncoderEvent::CaptureFailed(reason)) => {
                        self.command(EncoderCommand::Clear);
                        let stop = ControlMessage::new(Body::StreamStop(StreamStop { stream_id }));
                        if ch.send(&stop).await.is_ok() {
                            let _ = ch.close("capture failed").await;
                        }
                        return SessionEnd::CaptureFailed(reason);
                    }
                    None => {
                        let _ = ch.close("encoder stopped").await;
                        return SessionEnd::Fatal(CoreError::Closed);
                    }
                },
                _ = wait_stop(&mut stop) => {
                    self.command(EncoderCommand::Clear);
                    let bye = ControlMessage::new(Body::StreamStop(StreamStop { stream_id }));
                    if ch.send(&bye).await.is_ok() {
                        let _ = ch.close("sender stopped").await;
                    }
                    return SessionEnd::Stopped;
                }
            }
        }
    }

    /// Handles one control message while streaming; `Some` ends the session.
    async fn on_message(
        &mut self,
        ch: &mut ControlChannel,
        msg: ControlMessage,
        stream_id: u32,
        media_port: u16,
    ) -> Option<SessionEnd> {
        match msg.body? {
            Body::Pong(p) => {
                let now = self.now_us();
                if now >= p.t_us {
                    let rtt_ms = (now - p.t_us) as f32 / 1000.0;
                    self.status.send_modify(|s| s.rtt_ms = rtt_ms);
                }
            }
            Body::Ping(p) => {
                let pong = ControlMessage::new(Body::Pong(Pong {
                    nonce: p.nonce,
                    t_us: p.t_us,
                }));
                if let Err(e) = ch.send(&pong).await {
                    return Some(end_with(e, true));
                }
            }
            Body::Stats(s) if s.stream_id == stream_id => {
                if s.loss_pct >= crate::hub::NO_MEDIA_LOSS_PCT {
                    self.no_media_reports += 1;
                    if self.no_media_reports == NO_MEDIA_REPORTS {
                        let message = format!(
                            "the hub receives no audio from this device (is UDP port \
                             {media_port} blocked by a firewall on the hub or the network?)"
                        );
                        tracing::warn!("{message}");
                        self.emit(SenderEvent::Error(message));
                    }
                } else {
                    self.no_media_reports = 0;
                }
                let before = self.adapter.current();
                let after = self.adapter.on_report(s.loss_pct, Instant::now());
                if after != before {
                    tracing::debug!(
                        ?before,
                        ?after,
                        loss = s.loss_pct,
                        "adapting to reported loss"
                    );
                    self.command(EncoderCommand::Adapt(after));
                }
                self.status.send_modify(|st| {
                    st.loss_pct = if s.loss_pct.is_finite() {
                        s.loss_pct
                    } else {
                        0.0
                    };
                    st.bitrate = after.bitrate;
                });
            }
            Body::SetVolume(v) if v.stream_id == stream_id => {
                self.hub_control.0 = v.gain;
                self.emit_hub_control();
            }
            Body::SetMute(m) if m.stream_id == stream_id => {
                self.hub_control.1 = m.muted;
                self.emit_hub_control();
            }
            Body::SetPriority(p) if p.stream_id == stream_id => {
                self.hub_control.2 = p.priority;
                self.emit_hub_control();
            }
            Body::StreamStop(s) if s.stream_id == stream_id => {
                return Some(SessionEnd::Retry {
                    reason: "the hub stopped the stream".into(),
                    streamed: true,
                });
            }
            Body::StreamRejected(r) if r.stream_id == stream_id => {
                return Some(SessionEnd::Retry {
                    reason: format!("the hub rejected the stream: {}", r.reason),
                    streamed: true,
                });
            }
            Body::Bye(b) => {
                return Some(SessionEnd::Retry {
                    reason: format!("the hub closed the connection: {}", b.reason),
                    streamed: true,
                });
            }
            _ => {}
        }
        None
    }

    fn emit_hub_control(&self) {
        let (gain, muted, priority) = self.hub_control;
        self.emit(SenderEvent::HubControl {
            gain,
            muted,
            priority,
        });
    }
}

/// Finds a hub by device id or name over mDNS (see [`HubAddress::Discover`]).
async fn discover(
    target: &str,
    trust: &TrustStore,
) -> Result<(Vec<SocketAddr>, Option<[u8; 32]>, Option<String>)> {
    let mut browser = crate::discovery::browse()?;
    let by_id = hfa_proto::is_fingerprint(target);
    let wanted = target.to_lowercase();
    let mut deadline = tokio::time::Instant::now() + DISCOVER_TIMEOUT;
    let mut best: Option<crate::HubInfo> = None;
    while let Ok(Some(event)) = tokio::time::timeout_at(deadline, browser.recv()).await {
        let crate::DiscoveryEvent::Found(info) = event else {
            continue;
        };
        let matches = if by_id {
            info.device_id == target
        } else {
            info.name.to_lowercase() == wanted
        };
        if !matches || info.addrs.is_empty() {
            continue;
        }
        if by_id || trust.get(&info.device_id).is_some() {
            best = Some(info);
            break;
        }
        if best.is_none() {
            // An untrusted name match: give a trusted hub of the same name a moment.
            deadline = deadline.min(tokio::time::Instant::now() + Duration::from_secs(1));
            best = Some(info);
        }
    }
    let info = best.ok_or_else(|| CoreError::HubNotFound(target.to_owned()))?;
    let mut addrs: Vec<SocketAddr> = info
        .addrs
        .iter()
        .map(|ip| SocketAddr::new(*ip, info.port))
        .collect();
    addrs.sort_by_key(SocketAddr::is_ipv6);
    let key = trust.get(&info.device_id).map(|p| p.public_key);
    Ok((addrs, key, by_id.then(|| target.to_owned())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_and_key_errors_are_final_others_retry() {
        assert!(matches!(
            end_with(CoreError::PairingRequired, false),
            SessionEnd::Fatal(_)
        ));
        assert!(matches!(
            end_with(CoreError::PairingFailed("x".into()), true),
            SessionEnd::Fatal(_)
        ));
        assert!(matches!(
            end_with(CoreError::KeyMismatch("x".into()), false),
            SessionEnd::Fatal(_)
        ));
        assert!(matches!(
            end_with(CoreError::SelfConnection, false),
            SessionEnd::Fatal(_)
        ));
        for e in [
            CoreError::Closed,
            CoreError::Io("refused".into()),
            CoreError::Timeout("x".into()),
            CoreError::Rejected("full".into()),
            CoreError::HubNotFound("x".into()),
        ] {
            assert!(matches!(
                end_with(e, true),
                SessionEnd::Retry { streamed: true, .. }
            ));
        }
    }

    #[test]
    fn open_capture_passes_non_system_targets_through() {
        let (capture, warning) =
            open_capture(&CaptureTarget::Tone { freq_hz: 440.0 }).expect("tone");
        assert!(warning.is_none());
        assert_eq!(capture.format(), AudioFormat::INTERNAL);
    }
}
