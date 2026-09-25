//! Media encryption: ChaCha20-Poly1305 over each datagram.
//!
//! - Nonce (12 bytes) = `stream_id` BE ‖ `seq` BE ‖ `0u32`.
//! - AAD = the 16-byte encoded [`MediaHeader`].
//! - Output datagram = `header ‖ ciphertext ‖ tag` (tag = [`crate::AEAD_TAG_LEN`] bytes).
//!
//! Every stream uses a fresh [`MediaKey`] (sent inside the Noise-protected `StreamStart`).
//! `seq` is strictly increasing for the lifetime of a key: it never resets (not even with
//! [`crate::FLAG_RESET`]) and never wraps, so a `(key, nonce)` pair is never reused. A stream
//! that must restart at `seq = 0` gets a new `stream_id` *and* a new key.
//!
//! Replay protection: [`MediaOpener`] keeps a [`ReplayWindow`] and rejects datagrams whose
//! `seq` was already accepted or is older than [`crate::replay::REPLAY_WINDOW`] packets.

use std::fmt;

use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::media::MediaHeader;
use crate::replay::ReplayWindow;
use crate::Result;

/// A 256-bit media key for one stream. The bytes are wiped on drop; `Debug` never prints them.
#[derive(Clone, PartialEq, Eq, Zeroize, ZeroizeOnDrop)]
pub struct MediaKey([u8; 32]);

impl MediaKey {
    /// Generates a new random key from the OS CSPRNG.
    pub fn generate() -> Self {
        todo!("feat/proto")
    }

    /// Wraps existing key bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Builds a key from a slice (e.g. the `media_key` field of `StreamStart`).
    ///
    /// # Errors
    /// [`crate::ProtoError::InvalidKey`] if `bytes` is not exactly 32 bytes long.
    pub fn from_slice(bytes: &[u8]) -> Result<Self> {
        let arr: [u8; 32] = bytes.try_into().map_err(|_| {
            crate::ProtoError::InvalidKey(format!("expected 32 bytes, got {}", bytes.len()))
        })?;
        Ok(Self(arr))
    }

    /// The raw key bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Debug for MediaKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MediaKey(<redacted>)")
    }
}

/// Encrypts datagrams of one stream (sender side).
pub struct MediaSealer {
    key: MediaKey,
    stream_id: u32,
}

impl MediaSealer {
    /// Creates a sealer for `stream_id` using `key`.
    pub fn new(key: &MediaKey, stream_id: u32) -> Self {
        Self {
            key: key.clone(),
            stream_id,
        }
    }

    /// The stream this sealer belongs to.
    pub fn stream_id(&self) -> u32 {
        self.stream_id
    }

    /// Seals `payload` and writes `header ‖ ciphertext ‖ tag` into `out` (cleared first).
    ///
    /// # Errors
    /// [`crate::ProtoError::StreamMismatch`] if `header.stream_id` differs from this sealer's,
    /// [`crate::ProtoError::FrameTooLarge`] if the datagram would exceed [`crate::MAX_DATAGRAM`],
    /// [`crate::ProtoError::Crypto`] on AEAD failure.
    pub fn seal(&self, _header: &MediaHeader, _payload: &[u8], _out: &mut Vec<u8>) -> Result<()> {
        let _ = &self.key;
        todo!("feat/proto")
    }
}

/// Decrypts and authenticates datagrams of one stream (hub side), with replay protection.
pub struct MediaOpener {
    key: MediaKey,
    stream_id: u32,
    replay: ReplayWindow,
}

impl MediaOpener {
    /// Creates an opener for `stream_id` using `key`.
    pub fn new(key: &MediaKey, stream_id: u32) -> Self {
        Self {
            key: key.clone(),
            stream_id,
            replay: ReplayWindow::new(),
        }
    }

    /// The stream this opener belongs to.
    pub fn stream_id(&self) -> u32 {
        self.stream_id
    }

    /// Parses the header, checks the stream id, rejects replays, authenticates and decrypts
    /// the payload.
    ///
    /// Order: decode header → stream id → [`ReplayWindow::check`] (cheap, before decryption)
    /// → AEAD open → [`ReplayWindow::accept`] (only after authentication succeeded, so forged
    /// datagrams can never advance or poison the window). `FLAG_RESET` does not reset the
    /// window.
    ///
    /// # Errors
    /// Header errors from [`MediaHeader::decode`], [`crate::ProtoError::StreamMismatch`],
    /// [`crate::ProtoError::Replay`] for a duplicate or too old `seq`, or
    /// [`crate::ProtoError::Crypto`] if authentication fails.
    pub fn open(&mut self, _datagram: &[u8]) -> Result<(MediaHeader, Vec<u8>)> {
        let _ = (&self.key, &self.replay);
        todo!("feat/proto")
    }
}
