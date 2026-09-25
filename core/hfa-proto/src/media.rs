//! UDP media header.
//!
//! Byte layout (all integers big-endian), [`crate::MEDIA_HEADER_LEN`] = 16 bytes:
//!
//! | offset | size | field |
//! |---|---|---|
//! | 0 | 2 | magic `"HF"` ([`crate::MAGIC`]) |
//! | 2 | 1 | version ([`crate::PROTOCOL_VERSION`]) |
//! | 3 | 1 | flags ([`FLAG_FEC`], [`FLAG_DTX`], [`FLAG_RESET`]) |
//! | 4 | 4 | `stream_id` |
//! | 8 | 4 | `seq` |
//! | 12 | 4 | `timestamp` (48 kHz sample clock) |

use crate::{Result, MEDIA_HEADER_LEN};

/// The payload carries Opus in-band FEC data for the previous frame.
pub const FLAG_FEC: u8 = 0x01;
/// Silence keep-alive (discontinuous transmission). The payload is empty.
pub const FLAG_DTX: u8 = 0x02;
/// The sender restarted the stream (sequence/timestamp discontinuity); the hub must reset
/// the jitter buffer and decoder state for this stream.
pub const FLAG_RESET: u8 = 0x04;

/// The plaintext header in front of every media datagram. It is authenticated as AEAD
/// associated data but not encrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct MediaHeader {
    /// Bit set of `FLAG_*` values.
    pub flags: u8,
    /// Random per-stream identifier chosen by the sender.
    pub stream_id: u32,
    /// Packet counter, +1 per datagram. Also part of the AEAD nonce, so never reused per key.
    pub seq: u32,
    /// Media timestamp of the first sample in the packet, in 48 kHz samples.
    pub timestamp: u32,
}

impl MediaHeader {
    /// Returns `true` if every bit of `flag` is set in [`MediaHeader::flags`].
    pub fn has_flag(&self, flag: u8) -> bool {
        self.flags & flag == flag
    }

    /// Serializes the header (magic, version, flags, stream_id, seq, timestamp).
    pub fn encode(&self) -> [u8; MEDIA_HEADER_LEN] {
        todo!("feat/proto")
    }

    /// Parses a header from the start of `buf` and returns it together with the rest of the
    /// buffer (the payload).
    ///
    /// # Errors
    /// [`crate::ProtoError::Truncated`], [`crate::ProtoError::BadMagic`] or
    /// [`crate::ProtoError::UnsupportedVersion`].
    pub fn decode(_buf: &[u8]) -> Result<(MediaHeader, &[u8])> {
        todo!("feat/proto")
    }
}
