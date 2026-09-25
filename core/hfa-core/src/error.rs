//! The single error type of `hfa-core`.

use hfa_audio::AudioError;
use hfa_capture::CaptureError;
use hfa_proto::ProtoError;
use thiserror::Error;

/// Every error `hfa-core` can return. `Clone` so it can be broadcast in events.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CoreError {
    /// Network or file I/O failed.
    #[error("i/o error: {0}")]
    Io(String),
    /// Reading/writing JSON (settings, identity, trust store) failed.
    #[error("json error: {0}")]
    Json(String),
    /// Invalid settings or arguments.
    #[error("configuration error: {0}")]
    Config(String),
    /// Wire-format, crypto or Noise error.
    #[error(transparent)]
    Proto(#[from] ProtoError),
    /// Codec/DSP error.
    #[error(transparent)]
    Audio(#[from] AudioError),
    /// Capture/output error.
    #[error(transparent)]
    Capture(#[from] CaptureError),
    /// mDNS advertising/browsing failed.
    #[error("discovery error: {0}")]
    Discovery(String),
    /// The peer is unknown and no pairing secret was given / no pairing window is open.
    #[error("pairing required")]
    PairingRequired,
    /// Pairing failed (wrong PIN/token, expired window, too many attempts).
    #[error("pairing failed: {0}")]
    PairingFailed(String),
    /// The peer's static key does not match the pinned key (possible impersonation).
    #[error("peer key mismatch for {0}")]
    KeyMismatch(String),
    /// The peer violated the control protocol (unexpected message, bad version...).
    #[error("protocol error: {0}")]
    Protocol(String),
    /// The hub rejected a stream.
    #[error("stream rejected: {0}")]
    Rejected(String),
    /// A hub could not be found (discovery by name/id).
    #[error("hub not found: {0}")]
    HubNotFound(String),
    /// An operation timed out.
    #[error("timed out: {0}")]
    Timeout(String),
    /// The connection or engine is closed.
    #[error("closed")]
    Closed,
    /// A media stream used its last sequence number (`u32::MAX`); `seq` never wraps, so the
    /// sender must start a new stream (new id and key).
    #[error("sequence numbers of stream {0} exhausted")]
    SequenceExhausted(u32),
    /// A stream id / source is unknown.
    #[error("unknown stream {0}")]
    UnknownStream(u32),
}

impl From<std::io::Error> for CoreError {
    fn from(e: std::io::Error) -> Self {
        CoreError::Io(e.to_string())
    }
}

impl From<serde_json::Error> for CoreError {
    fn from(e: serde_json::Error) -> Self {
        CoreError::Json(e.to_string())
    }
}
