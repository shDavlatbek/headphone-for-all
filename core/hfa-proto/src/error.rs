//! The single error type of `hfa-proto`.

use thiserror::Error;

/// Every error `hfa-proto` can return.
///
/// Payloads are plain strings/integers so the error is `Clone + PartialEq` and can be
/// forwarded through channels and compared in tests.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProtoError {
    /// The buffer is shorter than the structure being decoded.
    #[error("truncated input: needed {needed} bytes, got {got}")]
    Truncated {
        /// Minimum number of bytes required.
        needed: usize,
        /// Number of bytes available.
        got: usize,
    },
    /// The datagram does not start with [`crate::MAGIC`].
    #[error("bad magic bytes")]
    BadMagic,
    /// The peer speaks a protocol version we do not support.
    #[error("unsupported protocol version {0}")]
    UnsupportedVersion(u8),
    /// A datagram's `stream_id` does not match the opener/sealer it was given to.
    #[error("stream id mismatch: expected {expected}, got {got}")]
    StreamMismatch {
        /// Stream id the opener/sealer was created for.
        expected: u32,
        /// Stream id found in the header.
        got: u32,
    },
    /// AEAD sealing or opening failed (wrong key, tampered data, or bad tag).
    #[error("authentication failed")]
    Crypto,
    /// A media datagram's `seq` was already received or is older than the replay window.
    #[error("replayed or too old media packet (seq {seq})")]
    Replay {
        /// The rejected sequence number.
        seq: u32,
    },
    /// A key or other fixed-length value had the wrong length.
    #[error("invalid key: {0}")]
    InvalidKey(String),
    /// A message or frame exceeds the allowed size.
    #[error("frame too large: {len} bytes (max {max})")]
    FrameTooLarge {
        /// Actual length.
        len: usize,
        /// Maximum allowed length.
        max: usize,
    },
    /// Protobuf decoding failed.
    #[error("protobuf decode error: {0}")]
    Decode(String),
    /// A control message was well-formed but semantically invalid (e.g. empty `oneof`).
    #[error("invalid message: {0}")]
    InvalidMessage(String),
    /// The Noise handshake or transport failed.
    #[error("noise error: {0}")]
    Noise(String),
    /// Pairing (SPAKE2 or confirmation MAC) failed.
    #[error("pairing failed: {0}")]
    Pairing(String),
    /// A pairing URI could not be parsed.
    #[error("invalid pairing uri: {0}")]
    InvalidUri(String),
}
