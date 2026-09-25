//! The error type of `hfa-ffi`.
//!
//! The flutter_rust_bridge API converts it into `anyhow::Error` (Dart: `AnyhowException`
//! whose message is the `Display` text below); the C ABI and JNI map it to `HFA_ERR_*` codes.

use hfa_audio::AudioError;
use hfa_capture::CaptureError;
use hfa_core::CoreError;
use thiserror::Error;

/// Every error an FFI entry point can report.
#[derive(Debug, Error)]
pub enum FfiError {
    /// `init_app` has not been called (or failed).
    #[error("not initialized: call init_app first")]
    NotInitialized,
    /// A caller-supplied value is invalid.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// The hub is not running.
    #[error("the hub is not running")]
    HubNotRunning,
    /// A sender is already running (stop it first).
    #[error("a sender is already running; stop it first")]
    SenderRunning,
    /// Engine, network, identity or settings error.
    #[error(transparent)]
    Core(#[from] CoreError),
    /// Capture or output error.
    #[error(transparent)]
    Capture(#[from] CaptureError),
    /// DSP error (PCM conversion).
    #[error(transparent)]
    Audio(#[from] AudioError),
    /// File-system error (creating the data directory).
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// Internal failure (runtime creation, a panicking engine task...).
    #[error("internal error: {0}")]
    Internal(String),
}

/// Result type of this crate.
pub type Result<T> = std::result::Result<T, FfiError>;

/// Human-readable text of a caught panic payload.
pub(crate) fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_readable() {
        assert_eq!(
            FfiError::NotInitialized.to_string(),
            "not initialized: call init_app first"
        );
        let core: FfiError = CoreError::PairingRequired.into();
        assert_eq!(core.to_string(), "pairing required");
        let any: anyhow::Error = FfiError::InvalidArgument("port".into()).into();
        assert_eq!(any.to_string(), "invalid argument: port");
    }

    #[test]
    fn panic_payloads_are_described() {
        let a = std::panic::catch_unwind(|| panic!("static text")).unwrap_err();
        assert_eq!(panic_message(a.as_ref()), "static text");
        let b = std::panic::catch_unwind(|| panic!("formatted {}", 7)).unwrap_err();
        assert_eq!(panic_message(b.as_ref()), "formatted 7");
        let c = std::panic::catch_unwind(|| std::panic::panic_any(42_u8)).unwrap_err();
        assert_eq!(panic_message(c.as_ref()), "unknown panic payload");
    }
}
