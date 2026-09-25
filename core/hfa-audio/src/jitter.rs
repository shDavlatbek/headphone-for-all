//! Adaptive jitter buffer (one per incoming stream on the hub).
//!
//! Packets are pushed as they arrive (any order) and popped one frame at a time in `seq`
//! order by the mixer thread. The target delay adapts to the RFC 3550 interarrival jitter
//! estimate: `target = clamp(frame_ms + 3·jitter, min_target_ms, max_target_ms)`.
//! Sequence numbers wrap around (`u32`), comparisons are wrap-aware.

use serde::{Deserialize, Serialize};

/// Jitter buffer configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct JitterConfig {
    /// Duration of one packet/frame in ms.
    pub frame_ms: u32,
    /// Lower bound of the adaptive target delay in ms.
    pub min_target_ms: u32,
    /// Upper bound of the adaptive target delay in ms.
    pub max_target_ms: u32,
    /// Target delay before the first jitter estimate, in ms.
    pub initial_target_ms: u32,
    /// Maximum number of buffered packets; further pushes return [`PushResult::Overflow`].
    pub capacity: usize,
}

impl Default for JitterConfig {
    /// 10 ms frames, target 20..=150 ms starting at 40 ms, 64 packets.
    fn default() -> Self {
        Self {
            frame_ms: 10,
            min_target_ms: 20,
            max_target_ms: 150,
            initial_target_ms: 40,
            capacity: 64,
        }
    }
}

/// Outcome of [`JitterBuffer::push`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PushResult {
    /// Stored.
    Accepted,
    /// A packet with this `seq` is already buffered.
    Duplicate,
    /// The packet's `seq` was already played out (or concealed).
    TooLate,
    /// The buffer is full; the packet was dropped.
    Overflow,
}

/// Outcome of [`JitterBuffer::pop`]: exactly one frame slot per call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pop {
    /// The expected packet.
    Packet(Vec<u8>),
    /// The expected packet is missing. `next` is a copy of the following packet if it is
    /// already buffered, so the caller can recover the lost frame with Opus FEC.
    Missing {
        /// Copy of the packet with `seq + 1`, if buffered (it stays in the buffer).
        next: Option<Vec<u8>>,
    },
    /// Not primed or ran dry: the caller should output silence/PLC and wait.
    Underrun,
}

/// Cumulative statistics.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct JitterStats {
    /// Packets accepted.
    pub received: u64,
    /// Frames reported as [`Pop::Missing`].
    pub lost: u64,
    /// Packets rejected as [`PushResult::TooLate`].
    pub late: u64,
    /// Packets rejected as [`PushResult::Duplicate`].
    pub duplicate: u64,
    /// Current RFC 3550 interarrival jitter estimate in ms.
    pub jitter_ms: f32,
}

/// The jitter buffer.
#[derive(Debug)]
pub struct JitterBuffer {
    config: JitterConfig,
}

impl JitterBuffer {
    /// Creates an empty buffer.
    pub fn new(config: JitterConfig) -> Self {
        Self { config }
    }

    /// The configuration.
    pub fn config(&self) -> &JitterConfig {
        &self.config
    }

    /// Inserts a packet. `timestamp` is the media timestamp (48 kHz samples) and
    /// `arrival_us` the local monotonic arrival time in microseconds.
    pub fn push(
        &mut self,
        _seq: u32,
        _timestamp: u32,
        _arrival_us: u64,
        _payload: Vec<u8>,
    ) -> PushResult {
        todo!("feat/audio")
    }

    /// Takes the next frame in `seq` order.
    pub fn pop(&mut self) -> Pop {
        todo!("feat/audio")
    }

    /// Buffered audio in ms (packets × frame_ms).
    pub fn buffered_ms(&self) -> f64 {
        todo!("feat/audio")
    }

    /// Current adaptive target delay in ms.
    pub fn target_ms(&self) -> f64 {
        todo!("feat/audio")
    }

    /// `true` once enough audio is buffered to start playout (reaching the target once).
    pub fn is_primed(&self) -> bool {
        todo!("feat/audio")
    }

    /// Cumulative statistics.
    pub fn stats(&self) -> JitterStats {
        todo!("feat/audio")
    }

    /// Drops all packets and state (e.g. on `FLAG_RESET`). Statistics are kept.
    pub fn reset(&mut self) {
        todo!("feat/audio")
    }
}
