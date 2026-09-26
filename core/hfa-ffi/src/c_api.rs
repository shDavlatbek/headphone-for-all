//! C ABI for the iOS ReplayKit broadcast upload extension (header:
//! `core/hfa-ffi/include/hfa_ext.h`, keep in sync).
//!
//! The extension (Swift, `app/ios/HfaBroadcast`) links the `hfa_ffi` static library and
//! calls:
//!
//! ```c
//! typedef struct HfaExtSender HfaExtSender;
//! HfaExtSender *hfa_ext_sender_start(const char *config_json);
//! int32_t hfa_ext_push_pcm(HfaExtSender *handle, const float *samples,
//!                          uint32_t frames, uint32_t channels, uint32_t rate);
//! int32_t hfa_ext_sender_stop(HfaExtSender *handle);
//! const char *hfa_ext_last_error(void);
//! int32_t hfa_ext_sender_state(HfaExtSender *handle, char *buf, uint32_t len);
//! ```
//!
//! `config_json` (unknown keys are ignored):
//!
//! ```json
//! {"data_dir": "/…/AppGroup/hfa", "hub_host": "192.168.1.20", "hub_port": 47810,
//!  "hub_device_id": "ab12-cd34-ef56-7890", "hub_key": "<base64url>", "label": "iPhone"}
//! ```
//!
//! - `data_dir` (required): the App Group directory the app initialized, so the extension
//!   uses the app's identity, settings and paired hubs. Pairing never happens here: the hub's
//!   key (`hub_key`, or the key of `hub_device_id`) must be in the trust store.
//! - `hub_host` (default `""`) / `hub_port` (default 0 = the port in the settings, or
//!   `hfa_proto::DEFAULT_PORT` when that is 0 too): an empty host means "find
//!   `hub_device_id` over mDNS", which is refused on iOS (no multicast entitlement there).
//! - `hub_device_id`, `hub_key` (optional, may be `null`): see `SenderStartDto`.
//! - `label` (default `"iOS audio"`): the stream label shown on the hub.
//!
//! Every function returns [`HFA_OK`] or a negative `HFA_ERR_*` code
//! (`hfa_ext_sender_start`: a handle or null) and never unwinds across the boundary: panics
//! are caught and reported as [`HFA_ERR_INTERNAL`]. After a failed call,
//! [`hfa_ext_last_error`] returns a description (thread-local: it describes the last call made
//! **on the calling thread**; every `hfa_ext_*` call clears it first, so it is null after a
//! successful call). The returned pointer stays valid until the next `hfa_ext_*` call on
//! that thread.
//!
//! Connection progress: [`hfa_ext_sender_start`] returns once the engine runs; the connection
//! is made in the background. [`hfa_ext_sender_state`] reports it (a small JSON document,
//! `failed` being final), so the extension can end the broadcast with the reason.
//!
//! PCM: the extension's feed is registered as 48 kHz stereo and every buffer is converted
//! ([`PcmConverter`]: `to_stereo`, plus a resampler when the rate is not 48 kHz; the converter
//! is rebuilt when rate or channels change mid-stream).

use std::cell::RefCell;
use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use hfa_audio::AudioFormat;
use hfa_capture::{CaptureTarget, ExternalFeed};
use hfa_core::{
    Identity, SenderConfig, SenderEngine, SenderEvent, SenderHandle, SenderState, SenderStatus,
    Settings, TrustStore,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;
use tokio::sync::broadcast;

use crate::error::panic_message;
use crate::hub_target::{self, HubRequest};
use crate::pcm::{format_is_valid, PcmConverter, MAX_SAMPLES_PER_CALL};
use crate::runtime::{block_on, build_runtime};
use crate::sender_meta::{self, SenderMeta};

/// Success.
pub const HFA_OK: i32 = 0;
/// A pointer argument was null or a numeric argument was out of range.
pub const HFA_ERR_INVALID_ARGUMENT: i32 = -1;
/// The configuration JSON could not be parsed or is incomplete.
pub const HFA_ERR_CONFIG: i32 = -2;
/// The engine failed (settings/identity I/O, capture, connection, internal error).
pub const HFA_ERR_ENGINE: i32 = -3;
/// No external feed with this id is registered (JNI `pushPcm` before `sender_start` or
/// after `sender_stop`).
pub const HFA_ERR_UNKNOWN_FEED: i32 = -4;
/// A panic was caught at the FFI boundary (a bug; see the last error / logs).
pub const HFA_ERR_INTERNAL: i32 = -5;
/// The function is not implemented in this build.
pub const HFA_ERR_NOT_IMPLEMENTED: i32 = -100;

/// Default label of the extension's stream.
pub const DEFAULT_EXT_LABEL: &str = "iOS audio";
/// Feed ids used by extension senders (one per handle), far away from ids the app picks.
const EXT_FEED_ID_BASE: u32 = 0xE7E7_0000;
/// How long `hfa_ext_sender_stop` waits for runtime tasks to wind down.
const RUNTIME_SHUTDOWN: Duration = Duration::from_secs(2);

thread_local! {
    static LAST_ERROR: RefCell<Option<CString>> = const { RefCell::new(None) };
}

fn set_last_error(message: &str) {
    // Interior NULs cannot be represented in a C string; replace them.
    let text = CString::new(message.replace('\0', " ")).unwrap_or_default();
    LAST_ERROR.with(|e| *e.borrow_mut() = Some(text));
}

fn clear_last_error() {
    LAST_ERROR.with(|e| {
        if let Ok(mut slot) = e.try_borrow_mut() {
            *slot = None;
        }
    });
}

/// An `HFA_ERR_*` code with its message.
#[derive(Debug)]
struct ExtError {
    code: i32,
    message: String,
}

impl ExtError {
    fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Runs `f` at the FFI boundary: clears the last error, catches panics, and records the
/// error message. Returns `f`'s value, or `on_error(code)`.
fn boundary<T>(on_error: impl FnOnce(i32) -> T, f: impl FnOnce() -> Result<T, ExtError>) -> T {
    clear_last_error();
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(value)) => value,
        Ok(Err(e)) => {
            set_last_error(&e.message);
            on_error(e.code)
        }
        Err(payload) => {
            let message = format!("internal panic: {}", panic_message(payload.as_ref()));
            // Logging may itself fail after a panic; never let that unwind.
            let _ = catch_unwind(|| tracing::error!("{message}"));
            set_last_error(&message);
            on_error(HFA_ERR_INTERNAL)
        }
    }
}

/// The extension's JSON configuration (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct ExtConfig {
    data_dir: PathBuf,
    #[serde(default)]
    hub_host: String,
    #[serde(default)]
    hub_port: u16,
    #[serde(default)]
    hub_device_id: Option<String>,
    #[serde(default)]
    hub_key: Option<String>,
    #[serde(default)]
    label: Option<String>,
}

/// Whether the extension can find a hub over mDNS. Not on iOS: mdns-sd needs the restricted
/// multicast entitlement there, which the broadcast extension does not have, so an empty
/// `hub_host` would reconnect forever without ever finding the hub.
const EXT_CAN_DISCOVER: bool = !cfg!(target_os = "ios");

impl ExtConfig {
    /// Parses and checks the fields that need no I/O.
    fn parse(json: &str) -> Result<Self, ExtError> {
        Self::parse_for(json, EXT_CAN_DISCOVER)
    }

    /// [`ExtConfig::parse`] for a platform where mDNS discovery works (`can_discover`) or not.
    fn parse_for(json: &str, can_discover: bool) -> Result<Self, ExtError> {
        let cfg: ExtConfig = serde_json::from_str(json)
            .map_err(|e| ExtError::new(HFA_ERR_CONFIG, format!("config json: {e}")))?;
        if cfg.data_dir.as_os_str().is_empty() {
            return Err(ExtError::new(HFA_ERR_CONFIG, "config: data_dir is empty"));
        }
        let has_id = cfg
            .hub_device_id
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty());
        if cfg.hub_host.trim().is_empty() && !has_id {
            return Err(ExtError::new(
                HFA_ERR_CONFIG,
                "config: hub_host or hub_device_id is required",
            ));
        }
        if cfg.hub_host.trim().is_empty() && !can_discover {
            return Err(ExtError::new(
                HFA_ERR_CONFIG,
                "config: hub_host is required (the hub cannot be found over mDNS here): \
                 enter the hub's address in the app",
            ));
        }
        if let Some(key) = cfg.hub_key.as_deref().filter(|k| !k.trim().is_empty()) {
            hub_target::decode_key(key)
                .map_err(|e| ExtError::new(HFA_ERR_CONFIG, format!("config: {e}")))?;
        }
        Ok(cfg)
    }

    fn label(&self) -> String {
        match self.label.as_deref().map(str::trim) {
            None | Some("") => DEFAULT_EXT_LABEL.to_owned(),
            Some(l) => l.to_owned(),
        }
    }
}

/// The extension never pairs, so the hub's key must be in the trust store: a key that is
/// merely known (a scanned URI whose pairing never completed, a hub forgotten since) would
/// make the engine ask for pairing and give up in the background.
fn check_hub_trusted(expected_key: Option<&[u8; 32]>, trust: &TrustStore) -> Result<(), ExtError> {
    match expected_key {
        Some(key) if trust.is_trusted(key) => Ok(()),
        _ => Err(ExtError::new(
            HFA_ERR_CONFIG,
            "the hub is not paired: pair it in the app first (pairing never happens in the extension)",
        )),
    }
}

/// Opaque handle to a running extension sender. Created by [`hfa_ext_sender_start`] and
/// destroyed by [`hfa_ext_sender_stop`].
///
/// [`hfa_ext_push_pcm`] and [`hfa_ext_sender_state`] only need shared access (the converter
/// has its own lock), so they may run on different threads at the same time.
pub struct HfaExtSender {
    runtime: Runtime,
    /// `None` only in unit tests (no engine).
    sender: Option<SenderHandle>,
    /// Facts from the engine's events (hub name, errors, hub controls), kept up to date by a
    /// forwarder task on `runtime`.
    meta: Arc<Mutex<SenderMeta>>,
    feed_id: u32,
    feed: ExternalFeed,
    converter: Mutex<PcmConverter>,
}

// The handle is used from the extension's threads through a raw pointer.
const _: () = {
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<HfaExtSender>();
};

fn next_ext_feed_id() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    EXT_FEED_ID_BASE.wrapping_add(NEXT.fetch_add(1, Ordering::Relaxed) & 0xFFFF)
}

fn engine_err(context: &str) -> impl Fn(hfa_core::CoreError) -> ExtError + '_ {
    move |e| ExtError::new(HFA_ERR_ENGINE, format!("{context}: {e}"))
}

fn start_ext_sender(cfg: &ExtConfig) -> Result<Box<HfaExtSender>, ExtError> {
    // Before anything that logs; the directory must exist for the log file.
    let _ = std::fs::create_dir_all(&cfg.data_dir);
    crate::logging::init_ext_logging(&cfg.data_dir);
    let settings = Settings::load_or_default(&cfg.data_dir).map_err(engine_err("settings"))?;
    let trust = TrustStore::load(&cfg.data_dir).map_err(engine_err("trust store"))?;
    let identity = Identity::load_or_create(&cfg.data_dir, &settings.device_name)
        .map_err(engine_err("identity"))?;
    let target = hub_target::resolve(
        &HubRequest {
            host: cfg.hub_host.clone(),
            port: cfg.hub_port,
            device_id: cfg.hub_device_id.clone(),
            key: cfg.hub_key.clone(),
        },
        settings.port,
        |id| trust.get(id).map(|p| p.public_key),
        &identity.public_key(),
    )
    .map_err(|e| ExtError::new(HFA_ERR_CONFIG, format!("config: {e}")))?;
    check_hub_trusted(target.expected_key.as_ref(), &trust)?;
    let runtime =
        build_runtime("hfa-ext").map_err(|e| ExtError::new(HFA_ERR_ENGINE, e.to_string()))?;
    let feed_id = next_ext_feed_id();
    let feed = hfa_capture::register_external(feed_id, AudioFormat::INTERNAL);
    let started = (|| {
        let capture = hfa_capture::open_capture(&CaptureTarget::External { id: feed_id })
            .map_err(|e| ExtError::new(HFA_ERR_ENGINE, format!("capture: {e}")))?;
        let config = SenderConfig {
            hub: target.address,
            settings,
            capture,
            label: cfg.label(),
            expected_hub_key: target.expected_key,
            pairing_secret: None,
        };
        block_on(&runtime, SenderEngine::start(config))
            .map_err(|e| ExtError::new(HFA_ERR_ENGINE, e.to_string()))?
            .map_err(engine_err("sender"))
    })();
    match started {
        Ok(sender) => {
            // Subscribed right after the start, so no event (e.g. `Connected`) is missed.
            let meta = Arc::new(Mutex::new(SenderMeta::new(sender.status(), None)));
            runtime.spawn(fold_events(sender.events(), Arc::clone(&meta)));
            Ok(Box::new(HfaExtSender {
                runtime,
                sender: Some(sender),
                meta,
                feed_id,
                feed,
                converter: Mutex::new(PcmConverter::new()),
            }))
        }
        Err(e) => {
            hfa_capture::unregister_external(feed_id);
            runtime.shutdown_timeout(RUNTIME_SHUTDOWN);
            Err(e)
        }
    }
}

/// Starts a sender that streams PCM pushed with [`hfa_ext_push_pcm`] to the configured hub.
/// Returns null on failure (see [`hfa_ext_last_error`]).
///
/// # Safety
/// `config_json` must be null or a valid, NUL-terminated C string that stays valid for the
/// duration of the call.
#[no_mangle]
pub unsafe extern "C" fn hfa_ext_sender_start(config_json: *const c_char) -> *mut HfaExtSender {
    boundary(
        |_| std::ptr::null_mut(),
        || {
            if config_json.is_null() {
                return Err(ExtError::new(
                    HFA_ERR_INVALID_ARGUMENT,
                    "config_json is null",
                ));
            }
            // SAFETY: non-null and NUL-terminated per the function contract.
            let json = unsafe { CStr::from_ptr(config_json) }
                .to_str()
                .map_err(|_| ExtError::new(HFA_ERR_CONFIG, "config_json is not UTF-8"))?;
            let cfg = ExtConfig::parse(json)?;
            let sender = start_ext_sender(&cfg)?;
            Ok(Box::into_raw(sender))
        },
    )
}

/// Pushes `frames` frames of interleaved `f32` PCM (`channels` channels at `rate` Hz).
/// Returns [`HFA_OK`] (also while the sender is still connecting; the audio is then
/// dropped) or a negative error code.
///
/// The format is given per call because ReplayKit only reveals it per buffer (44.1 or
/// 48 kHz, mono or stereo). `channels` must be 1..=8, `rate` 8000..=192000 and
/// `frames * channels` at most 1 536 000, otherwise [`HFA_ERR_INVALID_ARGUMENT`].
///
/// # Safety
/// `handle` must be null or a pointer returned by [`hfa_ext_sender_start`] that has not been
/// passed to [`hfa_ext_sender_stop`], and must not be used concurrently from another thread.
/// `samples` must be null or point to at least `frames * channels` readable `f32` values.
#[no_mangle]
pub unsafe extern "C" fn hfa_ext_push_pcm(
    handle: *mut HfaExtSender,
    samples: *const f32,
    frames: u32,
    channels: u32,
    rate: u32,
) -> i32 {
    boundary(
        |code| code,
        || {
            if handle.is_null() || samples.is_null() {
                return Err(ExtError::new(
                    HFA_ERR_INVALID_ARGUMENT,
                    "handle or samples is null",
                ));
            }
            if !format_is_valid(channels, rate) {
                return Err(ExtError::new(
                    HFA_ERR_INVALID_ARGUMENT,
                    format!("unsupported pcm format: {channels} ch, {rate} Hz"),
                ));
            }
            let len = (frames as usize)
                .checked_mul(channels as usize)
                .filter(|n| *n <= MAX_SAMPLES_PER_CALL)
                .ok_or_else(|| {
                    ExtError::new(
                        HFA_ERR_INVALID_ARGUMENT,
                        format!("buffer too large: {frames} frames x {channels} ch"),
                    )
                })?;
            // SAFETY: `handle` is a live pointer from `hfa_ext_sender_start` (function
            // contract); only shared access is needed.
            let sender = unsafe { &*handle };
            // SAFETY: `samples` points to `frames * channels` readable floats (contract).
            let pcm = unsafe { std::slice::from_raw_parts(samples, len) };
            sender.push(pcm, channels, rate)
        },
    )
}

impl HfaExtSender {
    fn push(&self, pcm: &[f32], channels: u32, rate: u32) -> Result<i32, ExtError> {
        let mut converter = self.converter.lock();
        let converted = converter
            .convert(pcm, channels, rate)
            .map_err(|e| ExtError::new(HFA_ERR_ENGINE, e.to_string()))?;
        self.feed.push(converted);
        Ok(HFA_OK)
    }

    /// The JSON document of [`hfa_ext_sender_state`].
    fn state_json(&self) -> String {
        let meta = self.meta.lock();
        // The handle's status is live; the events add what it lacks.
        let status = match &self.sender {
            Some(sender) => sender.status(),
            None => SenderStatus {
                state: SenderState::Stopped,
                ..meta.status.clone()
            },
        };
        let (state, _) = sender_meta::state_str(&status.state);
        let doc = ExtState {
            state,
            error: meta.error_for(&status.state),
            hub_name: meta.hub_name.as_deref(),
            bitrate: status.bitrate,
            loss_pct: status.loss_pct,
            rtt_ms: status.rtt_ms,
            level_db: status.level_db,
            hub_gain: meta.hub.gain,
            hub_muted: meta.hub.muted,
            hub_priority: meta.hub.priority,
        };
        // Serializing strings, numbers and booleans cannot fail (a non-finite float would
        // become `null`); keep a minimal document as the fallback anyway.
        serde_json::to_string(&doc).unwrap_or_else(|_| format!("{{\"state\":\"{state}\"}}"))
    }
}

/// The document written by [`hfa_ext_sender_state`].
#[derive(Serialize)]
struct ExtState<'a> {
    state: &'static str,
    error: Option<&'a str>,
    hub_name: Option<&'a str>,
    bitrate: u32,
    loss_pct: f32,
    rtt_ms: f32,
    level_db: f32,
    hub_gain: f32,
    hub_muted: bool,
    hub_priority: bool,
}

/// Folds the engine's events into `meta` until its channel closes (the engine stopped) or
/// the handle's runtime shuts down.
async fn fold_events(mut rx: broadcast::Receiver<SenderEvent>, meta: Arc<Mutex<SenderMeta>>) {
    loop {
        match rx.recv().await {
            Ok(event) => {
                if let SenderEvent::StateChanged(state) = &event {
                    tracing::info!(
                        state = sender_meta::state_str(state).0,
                        "extension sender state"
                    );
                }
                meta.lock().apply(event);
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {}
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// Writes the sender's current state as a NUL-terminated UTF-8 JSON object into `buf`:
///
/// ```json
/// {"state": "connecting|pairing|streaming|reconnecting|stopped|failed",
///  "error": "<failure reason when failed, else the last non-fatal error>" | null,
///  "hub_name": "Desk" | null, "bitrate": 128000, "loss_pct": 0.5, "rtt_ms": 3.2,
///  "level_db": -18.5, "hub_gain": 1.0, "hub_muted": false, "hub_priority": false}
/// ```
///
/// Returns the length of the JSON in bytes without the NUL (like `snprintf`). If that is
/// `>= len`, nothing was written except an empty string (when `len > 0`): call again with a
/// buffer of at least the returned length + 1. Negative: an `HFA_ERR_*` code.
///
/// `failed` is final (e.g. "pairing required", a key mismatch): the extension should end
/// the broadcast with the error. May be called from any thread, also while another thread
/// is inside [`hfa_ext_push_pcm`] with the same handle, but not concurrently with (or after)
/// [`hfa_ext_sender_stop`].
///
/// # Safety
/// `handle` must be null or a live pointer returned by [`hfa_ext_sender_start`]. `buf` must
/// be null (only with `len == 0`) or point to at least `len` writable bytes.
#[no_mangle]
pub unsafe extern "C" fn hfa_ext_sender_state(
    handle: *mut HfaExtSender,
    buf: *mut c_char,
    len: u32,
) -> i32 {
    boundary(
        |code| code,
        || {
            if handle.is_null() || (buf.is_null() && len > 0) {
                return Err(ExtError::new(
                    HFA_ERR_INVALID_ARGUMENT,
                    "handle or buf is null",
                ));
            }
            // SAFETY: a live pointer from `hfa_ext_sender_start` (contract); shared access.
            let sender = unsafe { &*handle };
            let json = sender.state_json();
            let needed = i32::try_from(json.len())
                .map_err(|_| ExtError::new(HFA_ERR_INTERNAL, "state document too large"))?;
            let len = len as usize;
            if len == 0 {
                return Ok(needed);
            }
            // SAFETY: `buf` points to `len` writable bytes (contract).
            let out = unsafe { std::slice::from_raw_parts_mut(buf.cast::<u8>(), len) };
            if json.len() < len {
                out[..json.len()].copy_from_slice(json.as_bytes());
                out[json.len()] = 0;
            } else {
                out[0] = 0;
            }
            Ok(needed)
        },
    )
}

/// Stops the sender and frees the handle. Returns [`HFA_OK`] or a negative error code (the
/// handle is freed in every case except a null `handle`).
///
/// # Safety
/// `handle` must be null or a pointer returned by [`hfa_ext_sender_start`]; it must not be
/// used again after this call.
#[no_mangle]
pub unsafe extern "C" fn hfa_ext_sender_stop(handle: *mut HfaExtSender) -> i32 {
    boundary(
        |code| code,
        || {
            if handle.is_null() {
                return Err(ExtError::new(HFA_ERR_INVALID_ARGUMENT, "handle is null"));
            }
            // SAFETY: a live pointer from `hfa_ext_sender_start`, never used again.
            let sender = unsafe { Box::from_raw(handle) };
            let HfaExtSender {
                runtime,
                sender,
                feed_id,
                ..
            } = *sender;
            let stopped = match sender {
                Some(sender) => block_on(&runtime, sender.stop()),
                None => Ok(()),
            };
            hfa_capture::unregister_external(feed_id);
            runtime.shutdown_timeout(RUNTIME_SHUTDOWN);
            stopped.map_err(|e| ExtError::new(HFA_ERR_ENGINE, e.to_string()))?;
            Ok(HFA_OK)
        },
    )
}

/// Describes the last failed `hfa_ext_*` call on the calling thread, or returns null if that
/// call succeeded. The string is owned by the library and valid until the next `hfa_ext_*`
/// call on this thread.
#[no_mangle]
pub extern "C" fn hfa_ext_last_error() -> *const c_char {
    LAST_ERROR.with(|e| match e.try_borrow() {
        Ok(slot) => slot.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()),
        Err(_) => std::ptr::null(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use hfa_capture::{open_capture, pcm_ring_with_channels};

    fn last_error() -> Option<String> {
        let p = hfa_ext_last_error();
        if p.is_null() {
            None
        } else {
            // SAFETY: non-null pointers from hfa_ext_last_error are valid C strings until the
            // next hfa_ext_* call on this thread.
            Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
        }
    }

    fn start(json: &str) -> *mut HfaExtSender {
        let c = CString::new(json).expect("no NUL");
        // SAFETY: valid C string.
        unsafe { hfa_ext_sender_start(c.as_ptr()) }
    }

    #[test]
    fn null_arguments_are_rejected_without_panicking() {
        let pcm = [0.0_f32; 4];
        // SAFETY: null pointers are explicitly allowed by every function's contract.
        unsafe {
            assert!(hfa_ext_sender_start(std::ptr::null()).is_null());
            assert!(last_error().expect("error").contains("null"));
            assert_eq!(
                hfa_ext_push_pcm(std::ptr::null_mut(), pcm.as_ptr(), 2, 2, 48_000),
                HFA_ERR_INVALID_ARGUMENT
            );
            assert_eq!(
                hfa_ext_sender_stop(std::ptr::null_mut()),
                HFA_ERR_INVALID_ARGUMENT
            );
            assert_eq!(last_error().as_deref(), Some("handle is null"));
        }
    }

    #[test]
    fn bad_configs_fail_before_any_io() {
        for (json, needle) in [
            ("not json", "config json"),
            ("{}", "data_dir"),
            (r#"{"data_dir": ""}"#, "data_dir is empty"),
            (r#"{"data_dir": "/x"}"#, "hub_host or hub_device_id"),
            (
                r#"{"data_dir": "/x", "hub_host": "h", "hub_key": "short"}"#,
                "hub key",
            ),
            (
                r#"{"data_dir": "/x", "hub_port": 70000, "hub_host": "h"}"#,
                "config json",
            ),
        ] {
            assert!(start(json).is_null(), "{json}");
            let err = last_error().expect("error set");
            assert!(err.contains(needle), "{json}: {err}");
        }
        let bytes = [0xFF_u8, 0xFE, 0];
        // SAFETY: NUL-terminated buffer.
        assert!(unsafe { hfa_ext_sender_start(bytes.as_ptr().cast()) }.is_null());
        assert_eq!(last_error().as_deref(), Some("config_json is not UTF-8"));
    }

    #[test]
    fn config_parsing() {
        let cfg = ExtConfig::parse(
            r#"{"data_dir": "/g/hfa", "hub_host": "10.0.0.2", "hub_port": 5000,
                "hub_device_id": null, "hub_key": null, "label": " iPad ", "future": 1}"#,
        )
        .expect("valid");
        assert_eq!(cfg.data_dir, PathBuf::from("/g/hfa"));
        assert_eq!(cfg.hub_port, 5000);
        assert_eq!(cfg.label(), "iPad");
        let cfg = ExtConfig::parse(r#"{"data_dir": "/g", "hub_device_id": "ab12-cd34-ef56-7890"}"#)
            .expect("discover by id");
        assert_eq!(cfg.hub_host, "");
        assert_eq!(cfg.hub_port, 0);
        assert_eq!(cfg.label(), DEFAULT_EXT_LABEL);
    }

    #[test]
    fn panics_are_caught_and_reported() {
        let code = boundary(|c| c, || -> Result<i32, ExtError> { panic!("kaboom") });
        assert_eq!(code, HFA_ERR_INTERNAL);
        assert_eq!(last_error().as_deref(), Some("internal panic: kaboom"));
        // The next successful call clears it.
        assert_eq!(boundary(|c| c, || Ok(HFA_OK)), HFA_OK);
        assert_eq!(last_error(), None);
        set_last_error("a\0b");
        assert_eq!(last_error().as_deref(), Some("a b"));
    }

    /// A handle without an engine: exercises validation, conversion and delivery into the
    /// feed exactly as `hfa_ext_push_pcm` does in the extension.
    #[test]
    fn push_converts_into_the_feed() {
        let feed_id = next_ext_feed_id();
        let feed = hfa_capture::register_external(feed_id, AudioFormat::INTERNAL);
        let mut source = open_capture(&CaptureTarget::External { id: feed_id }).expect("open");
        let (sink, mut ring) = pcm_ring_with_channels(96_000, 2);
        source.start(sink).expect("start");
        let probe = feed.clone();
        let handle = Box::into_raw(Box::new(HfaExtSender {
            runtime: build_runtime("hfa-ext-test").expect("runtime"),
            sender: None,
            meta: Arc::new(Mutex::new(SenderMeta::new(SenderStatus::default(), None))),
            feed_id,
            feed,
            converter: Mutex::new(PcmConverter::new()),
        }));
        let mono = [0.25_f32; 480];
        // SAFETY: `handle` is live until hfa_ext_sender_stop; buffers hold frames*channels.
        unsafe {
            assert_eq!(
                hfa_ext_push_pcm(handle, mono.as_ptr(), 480, 1, 48_000),
                HFA_OK
            );
            assert_eq!(last_error(), None);
            assert_eq!(
                hfa_ext_push_pcm(handle, mono.as_ptr(), 480, 9, 48_000),
                HFA_ERR_INVALID_ARGUMENT
            );
            assert_eq!(
                hfa_ext_push_pcm(handle, mono.as_ptr(), 480, 1, 7_000),
                HFA_ERR_INVALID_ARGUMENT
            );
            assert_eq!(
                hfa_ext_push_pcm(handle, mono.as_ptr(), u32::MAX, 8, 48_000),
                HFA_ERR_INVALID_ARGUMENT
            );
            assert!(last_error().expect("error").contains("too large"));
        }
        assert_eq!(ring.available(), 960, "mono 10 ms → stereo 480 frames");
        let mut out = vec![0.0_f32; 960];
        ring.pull(&mut out);
        assert!(out.iter().all(|&x| x == 0.25));
        // 44.1 kHz stereo is resampled to 48 kHz.
        let stereo = [0.1_f32; 4410 * 2];
        // SAFETY: as above.
        unsafe {
            assert_eq!(
                hfa_ext_push_pcm(handle, stereo.as_ptr(), 4410, 2, 44_100),
                HFA_OK
            );
        }
        let got = ring.available() / 2;
        assert!((4_000..=4_800).contains(&got), "{got} frames at 48 kHz");
        assert!(probe.is_attached());
        // SAFETY: live handle, not used afterwards.
        assert_eq!(unsafe { hfa_ext_sender_stop(handle) }, HFA_OK);
        assert!(!probe.is_attached(), "stop unregisters the feed");
        assert_eq!(probe.push(&[0.0; 2]), 0);
        source.stop();
    }

    /// Reads the state document through the C ABI (growing the buffer as told).
    fn state(handle: *mut HfaExtSender) -> serde_json::Value {
        let mut buf = vec![0 as c_char; 8];
        loop {
            // SAFETY: live handle; `buf` holds `buf.len()` writable bytes.
            let n = unsafe { hfa_ext_sender_state(handle, buf.as_mut_ptr(), buf.len() as u32) };
            assert!(n >= 0, "state failed: {n} {:?}", last_error());
            let n = n as usize;
            if n < buf.len() {
                // SAFETY: NUL-terminated by hfa_ext_sender_state.
                let text = unsafe { CStr::from_ptr(buf.as_ptr()) }
                    .to_str()
                    .expect("utf-8");
                assert_eq!(text.len(), n);
                return serde_json::from_str(text).expect("json");
            }
            // Too small: nothing but an empty string was written.
            assert_eq!(buf[0], 0);
            buf = vec![0 as c_char; n + 1];
        }
    }

    #[test]
    fn state_follows_snprintf_rules() {
        let feed_id = next_ext_feed_id();
        let feed = hfa_capture::register_external(feed_id, AudioFormat::INTERNAL);
        let handle = Box::into_raw(Box::new(HfaExtSender {
            runtime: build_runtime("hfa-ext-test").expect("runtime"),
            sender: None,
            meta: Arc::new(Mutex::new(SenderMeta::new(SenderStatus::default(), None))),
            feed_id,
            feed,
            converter: Mutex::new(PcmConverter::new()),
        }));
        // SAFETY: live handle; null buffers only with len 0.
        unsafe {
            let needed = hfa_ext_sender_state(handle, std::ptr::null_mut(), 0);
            assert!(needed > 20, "{needed}");
            assert_eq!(
                hfa_ext_sender_state(handle, std::ptr::null_mut(), 4),
                HFA_ERR_INVALID_ARGUMENT
            );
            assert_eq!(
                hfa_ext_sender_state(std::ptr::null_mut(), std::ptr::null_mut(), 0),
                HFA_ERR_INVALID_ARGUMENT
            );
            // Exactly the length without room for the NUL is too small.
            let mut buf = vec![b'x' as c_char; needed as usize];
            assert_eq!(
                hfa_ext_sender_state(handle, buf.as_mut_ptr(), buf.len() as u32),
                needed
            );
            assert_eq!(buf[0], 0);
        }
        handle_meta(handle).lock().apply(SenderEvent::HubControl {
            gain: 0.25,
            muted: true,
            priority: true,
        });
        let doc = state(handle);
        assert_eq!(doc["state"], "stopped");
        assert_eq!(doc["error"], serde_json::Value::Null);
        assert_eq!(doc["hub_gain"], 0.25);
        assert_eq!(doc["hub_muted"], true);
        assert_eq!(doc["hub_priority"], true);
        // SAFETY: live handle, not used afterwards.
        assert_eq!(unsafe { hfa_ext_sender_stop(handle) }, HFA_OK);
    }

    fn handle_meta(handle: *mut HfaExtSender) -> Arc<Mutex<SenderMeta>> {
        // SAFETY: a live handle created by the test.
        Arc::clone(&unsafe { &*handle }.meta)
    }

    fn b64url(key: &[u8; 32]) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(key)
    }

    /// A known but untrusted hub key is refused at start (the engine would otherwise ask
    /// for pairing and give up in the background).
    #[test]
    fn an_untrusted_hub_key_is_not_paired() {
        let dir = tempfile::tempdir().expect("tempdir");
        let json = serde_json::json!({
            "data_dir": dir.path(),
            "hub_host": "127.0.0.1",
            "hub_port": 9,
            "hub_key": b64url(&[7u8; 32]),
        });
        assert!(start(&json.to_string()).is_null());
        let err = last_error().expect("error");
        assert!(err.contains("not paired"), "{err}");
    }

    #[test]
    fn an_empty_host_is_refused_where_mdns_cannot_work() {
        let json = r#"{"data_dir": "/g", "hub_device_id": "ab12-cd34-ef56-7890"}"#;
        assert!(ExtConfig::parse_for(json, true).is_ok());
        let err = ExtConfig::parse_for(json, false).expect_err("no mDNS");
        assert_eq!(err.code, HFA_ERR_CONFIG);
        assert!(err.message.contains("hub_host is required"), "{err:?}");
        let with_host = r#"{"data_dir": "/g", "hub_host": "10.0.0.2"}"#;
        assert!(ExtConfig::parse_for(with_host, false).is_ok());
    }

    /// The headline case of the state query: the extension trusts the hub, but the hub does
    /// not know this device (it was forgotten there), so the engine fails with "pairing
    /// required" after the start succeeded. Only `hfa_ext_sender_state` can tell.
    #[test]
    fn a_hub_refusal_after_start_is_reported_as_failed() {
        let runtime = build_runtime("hfa-ext-hub").expect("runtime");
        let hub_dir = tempfile::tempdir().expect("tempdir");
        let mut hub_settings = Settings::load_or_default(hub_dir.path()).expect("settings");
        hub_settings.port = 0;
        hub_settings.device_name = "Desk".into();
        let hub = block_on(
            &runtime,
            hfa_core::HubEngine::start(hfa_core::HubConfig {
                settings: hub_settings,
                output: hfa_capture::open_output(&hfa_capture::OutputTarget::Null, 20)
                    .expect("null output"),
                advertise: false,
            }),
        )
        .expect("runtime")
        .expect("hub");
        let hub_key = Identity::load_or_create(hub_dir.path(), "Desk")
            .expect("hub identity")
            .public_key();

        let ext_dir = tempfile::tempdir().expect("tempdir");
        TrustStore::load(ext_dir.path())
            .expect("trust")
            .add(hfa_core::TrustedPeer::new(hub_key, "Desk"))
            .expect("trust the hub");
        let json = serde_json::json!({
            "data_dir": ext_dir.path(),
            "hub_host": "127.0.0.1",
            "hub_port": hub.local_port(),
            "hub_key": b64url(&hub_key),
        });
        let handle = start(&json.to_string());
        assert!(!handle.is_null(), "{:?}", last_error());
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let doc = loop {
            let doc = state(handle);
            if doc["state"] == "failed" || std::time::Instant::now() > deadline {
                break doc;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(doc["state"], "failed", "{doc}");
        let error = doc["error"].as_str().expect("error text");
        assert!(error.contains("pairing"), "{error}");
        // SAFETY: live handle, not used afterwards.
        assert_eq!(unsafe { hfa_ext_sender_stop(handle) }, HFA_OK);
        block_on(&runtime, hub.stop()).expect("runtime");
    }

    /// `include/hfa_ext.h` must declare the same codes and functions as this module.
    #[test]
    fn header_matches_the_rust_constants() {
        // A Windows checkout may have converted the header to CRLF line endings (the
        // repository's .gitattributes asks for LF, but a local `core.autocrlf` can differ).
        let header = include_str!("../include/hfa_ext.h").replace("\r\n", "\n");
        for (name, value) in [
            ("HFA_OK", HFA_OK),
            ("HFA_ERR_INVALID_ARGUMENT", HFA_ERR_INVALID_ARGUMENT),
            ("HFA_ERR_CONFIG", HFA_ERR_CONFIG),
            ("HFA_ERR_ENGINE", HFA_ERR_ENGINE),
            ("HFA_ERR_UNKNOWN_FEED", HFA_ERR_UNKNOWN_FEED),
            ("HFA_ERR_INTERNAL", HFA_ERR_INTERNAL),
            ("HFA_ERR_NOT_IMPLEMENTED", HFA_ERR_NOT_IMPLEMENTED),
        ] {
            let line = format!("#define {name} ({value})");
            assert!(header.contains(&line), "header lacks `{line}`");
        }
        for f in [
            "HfaExtSender *hfa_ext_sender_start(const char *config_json);",
            "int32_t hfa_ext_push_pcm(HfaExtSender *handle, const float *samples, uint32_t frames,\n                         uint32_t channels, uint32_t rate);",
            "int32_t hfa_ext_sender_stop(HfaExtSender *handle);",
            "const char *hfa_ext_last_error(void);",
            "int32_t hfa_ext_sender_state(HfaExtSender *handle, char *buf, uint32_t len);",
        ] {
            assert!(header.contains(f), "header lacks `{f}`");
        }
    }
}
