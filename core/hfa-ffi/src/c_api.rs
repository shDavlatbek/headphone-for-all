//! C ABI for the iOS ReplayKit broadcast upload extension (header:
//! `core/hfa-ffi/include/hfa_ext.h`, keep in sync).
//!
//! The extension (Swift, `app/ios/BroadcastExtension`) links the `hfa_ffi` static library and
//! calls:
//!
//! ```c
//! typedef struct HfaExtSender HfaExtSender;
//! HfaExtSender *hfa_ext_sender_start(const char *config_json);
//! int32_t hfa_ext_push_pcm(HfaExtSender *handle, const float *samples,
//!                          uint32_t frames, uint32_t channels, uint32_t rate);
//! int32_t hfa_ext_sender_stop(HfaExtSender *handle);
//! const char *hfa_ext_last_error(void);
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
//!   uses the app's identity, settings and paired hubs. Pairing never happens here: the hub
//!   must already be trusted (by `hub_key`, or by `hub_device_id` in the trust store).
//! - `hub_host` (default `""`) / `hub_port` (default 0 = the port in the settings): an empty
//!   host means "find `hub_device_id` over mDNS".
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
//! PCM: the extension's feed is registered as 48 kHz stereo and every buffer is converted
//! ([`PcmConverter`]: `to_stereo`, plus a resampler when the rate is not 48 kHz; the converter
//! is rebuilt when rate or channels change mid-stream).

use std::cell::RefCell;
use std::ffi::{c_char, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use hfa_audio::AudioFormat;
use hfa_capture::{CaptureTarget, ExternalFeed};
use hfa_core::{Identity, SenderConfig, SenderEngine, SenderHandle, Settings, TrustStore};
use serde::Deserialize;
use tokio::runtime::Runtime;

use crate::error::panic_message;
use crate::hub_target::{self, HubRequest};
use crate::pcm::{format_is_valid, PcmConverter, MAX_SAMPLES_PER_CALL};
use crate::runtime::{block_on, build_runtime};

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

impl ExtConfig {
    /// Parses and checks the fields that need no I/O.
    fn parse(json: &str) -> Result<Self, ExtError> {
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

/// Opaque handle to a running extension sender. Created by [`hfa_ext_sender_start`] and
/// destroyed by [`hfa_ext_sender_stop`].
pub struct HfaExtSender {
    runtime: Runtime,
    /// `None` only in unit tests (no engine).
    sender: Option<SenderHandle>,
    feed_id: u32,
    feed: ExternalFeed,
    converter: PcmConverter,
}

fn next_ext_feed_id() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    EXT_FEED_ID_BASE.wrapping_add(NEXT.fetch_add(1, Ordering::Relaxed) & 0xFFFF)
}

fn engine_err(context: &str) -> impl Fn(hfa_core::CoreError) -> ExtError + '_ {
    move |e| ExtError::new(HFA_ERR_ENGINE, format!("{context}: {e}"))
}

fn start_ext_sender(cfg: &ExtConfig) -> Result<Box<HfaExtSender>, ExtError> {
    crate::logging::init_tracing();
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
    if target.expected_key.is_none() {
        return Err(ExtError::new(
            HFA_ERR_CONFIG,
            "the hub is not paired: pair it in the app first (pairing never happens in the extension)",
        ));
    }
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
        Ok(sender) => Ok(Box::new(HfaExtSender {
            runtime,
            sender: Some(sender),
            feed_id,
            feed,
            converter: PcmConverter::new(),
        })),
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
            // SAFETY: `handle` is a live pointer from `hfa_ext_sender_start`, used by one
            // thread at a time (function contract).
            let sender = unsafe { &mut *handle };
            // SAFETY: `samples` points to `frames * channels` readable floats (contract).
            let pcm = unsafe { std::slice::from_raw_parts(samples, len) };
            sender.push(pcm, channels, rate)
        },
    )
}

impl HfaExtSender {
    fn push(&mut self, pcm: &[f32], channels: u32, rate: u32) -> Result<i32, ExtError> {
        let converted = self
            .converter
            .convert(pcm, channels, rate)
            .map_err(|e| ExtError::new(HFA_ERR_ENGINE, e.to_string()))?;
        self.feed.push(converted);
        Ok(HFA_OK)
    }
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
            feed_id,
            feed,
            converter: PcmConverter::new(),
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

    /// `include/hfa_ext.h` must declare the same codes and functions as this module.
    #[test]
    fn header_matches_the_rust_constants() {
        let header = include_str!("../include/hfa_ext.h");
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
        ] {
            assert!(header.contains(f), "header lacks `{f}`");
        }
    }
}
