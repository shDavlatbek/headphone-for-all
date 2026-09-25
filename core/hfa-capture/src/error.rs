//! The single error type of `hfa-capture`.

use hfa_audio::AudioError;
use thiserror::Error;

/// Every error `hfa-capture` can return.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CaptureError {
    /// The requested capture/output kind is not available on this platform or OS version.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// The OS denied access (e.g. macOS "System audio recording" permission).
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    /// A device, process or external feed was not found.
    #[error("not found: {0}")]
    NotFound(String),
    /// The device/stream format cannot be handled.
    #[error("unsupported format: {0}")]
    Format(String),
    /// `start` was called on a source/output that is already running.
    #[error("already running")]
    AlreadyRunning,
    /// An argument (e.g. a target string) is invalid.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// An OS audio API (cpal, WASAPI, PipeWire, Core Audio) failed.
    #[error("audio backend error: {0}")]
    Backend(String),
    /// File I/O failed.
    #[error("i/o error: {0}")]
    Io(String),
    /// An `hfa-audio` operation failed (e.g. WAV reading).
    #[error(transparent)]
    Audio(#[from] AudioError),
}

impl From<std::io::Error> for CaptureError {
    fn from(e: std::io::Error) -> Self {
        CaptureError::Io(e.to_string())
    }
}
