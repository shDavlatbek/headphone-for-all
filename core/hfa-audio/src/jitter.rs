//! Adaptive jitter buffer (one per incoming stream on the hub).
//!
//! Packets are pushed as they arrive (any order) and popped one frame at a time in `seq`
//! order by the mixer thread. The target delay adapts to the RFC 3550 interarrival jitter
//! estimate: `target = clamp(frame_ms + 3·jitter, min_target_ms, max_target_ms)`.
//! Sequence numbers wrap around (`u32`), comparisons are wrap-aware.
//!
//! # Semantics
//!
//! - **Priming.** Nothing is popped until [`JitterBuffer::is_primed`]: `buffered_ms() >=
//!   target_ms()`. Until then [`JitterBuffer::pop`] returns [`Pop::Underrun`]. When priming
//!   completes, leading gaps (packets that never arrived before the first buffered one) are
//!   skipped and counted as lost, so playout starts on a real packet.
//! - **Playout.** Once primed, every pop yields exactly one frame slot in `seq` order:
//!   [`Pop::Packet`], or [`Pop::Missing`] for a gap (with a copy of the following packet, if
//!   buffered, for Opus FEC). When the buffer runs completely dry the pop returns
//!   [`Pop::Underrun`] and the buffer **re-primes** (waits for the target level again).
//! - **Push results.** A packet whose `seq` was already played out (or skipped) is
//!   [`PushResult::TooLate`]; one already buffered is [`PushResult::Duplicate`]. Before the
//!   first pop, a packet older than the first buffered one is still accepted (start-up
//!   reordering). When storing a packet would exceed `capacity` packets, or would make the
//!   window span more than `capacity` sequence numbers (a jump ahead), the **oldest** buffered
//!   slots are dropped (their frames count as lost), the new packet is stored, leading gaps
//!   are skipped and [`PushResult::Overflow`] is returned (if the new packet is itself the
//!   oldest, it is the one dropped). A packet more than `capacity` behind the first
//!   buffered one before playout starts is dropped (also `Overflow`).
//! - **Adaptive target.** Every non-duplicate push updates the RFC 3550 §6.4.1 interarrival
//!   jitter `J += (|D| − J) / 16`, where `D` is the difference of relative transit times of
//!   consecutive arrivals (timestamps at 48 kHz, arrival times in µs). The raw target is
//!   `frame_ms + 3·J`; the target follows it upwards immediately and decays towards it with a
//!   time constant of [`TARGET_DECAY_MS`] of pushed audio, clamped to
//!   `min_target_ms..=max_target_ms` (and to `capacity · frame_ms`). Before two packets were
//!   seen the target is `initial_target_ms` (clamped the same way).
//! - [`JitterBuffer::reset`] drops packets and playout state but keeps the statistics and the
//!   jitter/target estimate (they describe the network, not the stream).

use std::collections::VecDeque;

use serde::{Deserialize, Serialize};

/// Time constant (in ms of pushed audio) of the target's slow decay towards the raw target.
pub const TARGET_DECAY_MS: f64 = 8_000.0;

/// Media timestamp rate (samples per second) used for the jitter estimate.
const TIMESTAMP_RATE_HZ: f64 = 48_000.0;

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
    /// Maximum number of buffered packets (and maximum `seq` span of the window); a push
    /// beyond it drops the oldest slots and returns [`PushResult::Overflow`].
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
    /// The buffer was full (or the packet jumped too far ahead): the **oldest** buffered
    /// slots were dropped to make room and the new packet was stored. Also returned, with the
    /// packet dropped, for a start-up packet too far behind the window.
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
    /// Frames lost: reported as [`Pop::Missing`], never-received slots skipped when playout
    /// starts on the first real packet, and slots (received or not) dropped by an overflow.
    pub lost: u64,
    /// Packets rejected as [`PushResult::TooLate`].
    pub late: u64,
    /// Packets rejected as [`PushResult::Duplicate`].
    pub duplicate: u64,
    /// Current RFC 3550 interarrival jitter estimate in ms.
    pub jitter_ms: f32,
}

/// `seq + n` (wrapping); `seq` is only `None` before the first packet, where 0 is used.
fn next_after(seq: Option<u32>, n: usize) -> u32 {
    seq.unwrap_or(0).wrapping_add(n as u32)
}

/// The jitter buffer.
#[derive(Debug)]
pub struct JitterBuffer {
    config: JitterConfig,
    /// `slots[i]` holds the payload of packet `next_seq + i` (`None` = not received yet).
    /// Invariant: empty, or the last slot is `Some`.
    slots: VecDeque<Option<Vec<u8>>>,
    /// Sequence number of `slots[0]`; `None` until the first packet after `new`/`reset`.
    next_seq: Option<u32>,
    /// Number of `Some` slots.
    count: usize,
    /// Set once something was popped or skipped: `next_seq` can no longer move backwards.
    anchored: bool,
    primed: bool,
    stats: JitterStats,
    /// RFC 3550 jitter estimate in ms.
    jitter_ms: f64,
    /// `(arrival_ms, timestamp)` of the previous arrival, `None` before the first packet.
    prev_arrival: Option<(f64, u32)>,
    /// Current adaptive target in ms.
    target_ms: f64,
}

impl JitterBuffer {
    /// Creates an empty buffer.
    pub fn new(config: JitterConfig) -> Self {
        let mut jb = Self {
            config,
            slots: VecDeque::with_capacity(config.capacity.max(1)),
            next_seq: None,
            count: 0,
            anchored: false,
            primed: false,
            stats: JitterStats::default(),
            jitter_ms: 0.0,
            prev_arrival: None,
            target_ms: 0.0,
        };
        jb.target_ms = jb.clamp_target(f64::from(config.initial_target_ms));
        jb
    }

    /// The configuration.
    pub fn config(&self) -> &JitterConfig {
        &self.config
    }

    fn capacity(&self) -> usize {
        self.config.capacity.max(1)
    }

    fn clamp_target(&self, t: f64) -> f64 {
        let frame = f64::from(self.config.frame_ms);
        let hi = f64::from(self.config.max_target_ms).min(self.capacity() as f64 * frame);
        let lo = f64::from(self.config.min_target_ms).min(hi);
        t.clamp(lo, hi)
    }

    /// Updates the RFC 3550 jitter estimate and the adaptive target.
    fn update_jitter(&mut self, timestamp: u32, arrival_us: u64) {
        let arrival_ms = arrival_us as f64 / 1000.0;
        if let Some((prev_ms, prev_ts)) = self.prev_arrival {
            // D(i, j) = (Rj − Ri) − (Sj − Si); the timestamp difference is wrap-aware.
            let dts_ms =
                f64::from(timestamp.wrapping_sub(prev_ts) as i32) * 1000.0 / TIMESTAMP_RATE_HZ;
            let d = ((arrival_ms - prev_ms) - dts_ms).abs();
            self.jitter_ms += (d - self.jitter_ms) / 16.0;
            let raw = self.clamp_target(f64::from(self.config.frame_ms) + 3.0 * self.jitter_ms);
            if raw >= self.target_ms {
                self.target_ms = raw;
            } else {
                let alpha = (f64::from(self.config.frame_ms) / TARGET_DECAY_MS).min(1.0);
                self.target_ms = self.clamp_target(self.target_ms + (raw - self.target_ms) * alpha);
            }
        }
        self.prev_arrival = Some((arrival_ms, timestamp));
    }

    /// Drops the front slot, advancing `next_seq`. Returns the dropped payload.
    fn advance(&mut self) -> Option<Vec<u8>> {
        let slot = self.slots.pop_front().flatten();
        if slot.is_some() {
            self.count -= 1;
        }
        if let Some(n) = self.next_seq.as_mut() {
            *n = n.wrapping_add(1);
        }
        self.anchored = true;
        slot
    }

    /// Drops the oldest slot; its frame will never be played, so it counts as lost.
    fn drop_front(&mut self) {
        self.advance();
        self.stats.lost += 1;
    }

    /// Skips leading never-received slots (counted as lost).
    fn skip_leading_gaps(&mut self) {
        while matches!(self.slots.front(), Some(None)) {
            self.advance();
            self.stats.lost += 1;
        }
    }

    /// Marks the buffer primed once the target level is reached.
    fn check_primed(&mut self) {
        if !self.primed && self.count > 0 && self.buffered_ms() >= self.target_ms {
            self.primed = true;
            self.skip_leading_gaps();
        }
    }

    /// Inserts a packet. `timestamp` is the media timestamp (48 kHz samples) and
    /// `arrival_us` the local monotonic arrival time in microseconds.
    pub fn push(
        &mut self,
        seq: u32,
        timestamp: u32,
        arrival_us: u64,
        payload: Vec<u8>,
    ) -> PushResult {
        let cap = self.capacity();
        let Some(next) = self.next_seq else {
            // First packet after new/reset.
            self.update_jitter(timestamp, arrival_us);
            self.next_seq = Some(seq);
            self.slots.push_back(Some(payload));
            self.count = 1;
            self.stats.received += 1;
            self.check_primed();
            return PushResult::Accepted;
        };
        // Signed, wrap-aware distance from the first slot.
        let dist = seq.wrapping_sub(next) as i32;
        if dist < 0 {
            if self.anchored {
                self.stats.late += 1;
                self.update_jitter(timestamp, arrival_us);
                return PushResult::TooLate;
            }
            // Start-up reordering: extend the window backwards.
            let back = dist.unsigned_abs() as usize;
            if back + self.slots.len() > cap || self.count >= cap {
                return PushResult::Overflow;
            }
            self.update_jitter(timestamp, arrival_us);
            for _ in 1..back {
                self.slots.push_front(None);
            }
            self.slots.push_front(Some(payload));
            self.next_seq = Some(seq);
            self.count += 1;
            self.stats.received += 1;
            self.check_primed();
            return PushResult::Accepted;
        }
        let idx = dist as usize;
        if matches!(self.slots.get(idx), Some(Some(_))) {
            self.stats.duplicate += 1;
            return PushResult::Duplicate;
        }
        self.update_jitter(timestamp, arrival_us);
        // Make room: drop the oldest slots while the buffer is full or the window too wide.
        let mut idx = idx;
        let overflow = self.count >= cap || idx >= cap;
        while self.count >= cap {
            if idx == 0 {
                // The new packet is itself the oldest: it is the one dropped.
                return PushResult::Overflow;
            }
            self.drop_front();
            idx -= 1;
        }
        if idx >= cap {
            let mut shift = idx - (cap - 1);
            while shift > 0 && !self.slots.is_empty() {
                self.drop_front();
                shift -= 1;
            }
            if shift > 0 {
                // Jumped past everything buffered: the skipped sequence numbers are lost.
                self.next_seq = Some(next_after(self.next_seq, shift));
                self.stats.lost += shift as u64;
                self.anchored = true;
            }
            idx = cap - 1;
        }
        if idx >= self.slots.len() {
            self.slots.resize_with(idx + 1, || None);
        }
        self.slots[idx] = Some(payload);
        self.count += 1;
        self.stats.received += 1;
        if overflow {
            // Frames older than the kept window are gone anyway; do not add latency by
            // playing out their gaps.
            self.skip_leading_gaps();
        }
        self.check_primed();
        if overflow {
            PushResult::Overflow
        } else {
            PushResult::Accepted
        }
    }

    /// Takes the next frame in `seq` order.
    pub fn pop(&mut self) -> Pop {
        self.check_primed();
        if !self.primed {
            return Pop::Underrun;
        }
        if self.count == 0 {
            // Ran dry: re-prime before playing again.
            self.primed = false;
            return Pop::Underrun;
        }
        match self.advance() {
            Some(payload) => Pop::Packet(payload),
            None => {
                self.stats.lost += 1;
                let next = self.slots.front().and_then(|s| s.clone());
                Pop::Missing { next }
            }
        }
    }

    /// Buffered audio in ms (packets × frame_ms).
    pub fn buffered_ms(&self) -> f64 {
        self.count as f64 * f64::from(self.config.frame_ms)
    }

    /// Current adaptive target delay in ms.
    pub fn target_ms(&self) -> f64 {
        self.target_ms
    }

    /// `true` once enough audio is buffered to start playout (reaching the target once).
    /// Becomes `false` again after an [`Pop::Underrun`] caused by running dry.
    pub fn is_primed(&self) -> bool {
        self.primed
    }

    /// Cumulative statistics.
    pub fn stats(&self) -> JitterStats {
        JitterStats {
            jitter_ms: self.jitter_ms as f32,
            ..self.stats
        }
    }

    /// Drops all packets and state (e.g. on `FLAG_RESET`). Statistics are kept, as are the
    /// jitter estimate and adaptive target.
    pub fn reset(&mut self) {
        self.slots.clear();
        self.next_seq = None;
        self.count = 0;
        self.anchored = false;
        self.primed = false;
        self.prev_arrival = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 10 ms frames, fixed 30 ms target when timing is perfect, 8 packets.
    fn fixed_cfg() -> JitterConfig {
        JitterConfig {
            frame_ms: 10,
            min_target_ms: 30,
            max_target_ms: 150,
            initial_target_ms: 30,
            capacity: 8,
        }
    }

    /// Pushes `seq` with perfect timing (arrival = seq · 10 ms, timestamp = seq · 480).
    fn push(jb: &mut JitterBuffer, seq: u32) -> PushResult {
        let n = u64::from(seq);
        jb.push(
            seq,
            seq.wrapping_mul(480),
            n * 10_000,
            seq.to_be_bytes().to_vec(),
        )
    }

    fn pkt(seq: u32) -> Pop {
        Pop::Packet(seq.to_be_bytes().to_vec())
    }

    #[test]
    fn primes_then_plays_in_order_then_underruns() {
        let mut jb = JitterBuffer::new(fixed_cfg());
        assert_eq!(jb.target_ms(), 30.0);
        assert_eq!(jb.pop(), Pop::Underrun);
        assert_eq!(push(&mut jb, 0), PushResult::Accepted);
        assert_eq!(push(&mut jb, 1), PushResult::Accepted);
        assert!(!jb.is_primed());
        assert_eq!(jb.pop(), Pop::Underrun, "nothing is popped before priming");
        assert_eq!(jb.buffered_ms(), 20.0);
        assert_eq!(push(&mut jb, 2), PushResult::Accepted);
        assert!(jb.is_primed());
        for s in 0..3 {
            assert_eq!(jb.pop(), pkt(s));
        }
        assert_eq!(jb.buffered_ms(), 0.0);
        assert_eq!(jb.pop(), Pop::Underrun);
        assert!(!jb.is_primed(), "running dry re-primes");
        // Re-prime: one packet is not enough.
        push(&mut jb, 3);
        assert_eq!(jb.pop(), Pop::Underrun);
        push(&mut jb, 4);
        push(&mut jb, 5);
        assert!(jb.is_primed());
        assert_eq!(jb.pop(), pkt(3));
        let st = jb.stats();
        assert_eq!((st.received, st.lost, st.late, st.duplicate), (6, 0, 0, 0));
    }

    #[test]
    fn reorders_and_rejects_duplicates_and_late() {
        let mut jb = JitterBuffer::new(fixed_cfg());
        for s in [0, 2, 1, 3] {
            assert_eq!(push(&mut jb, s), PushResult::Accepted);
        }
        assert_eq!(push(&mut jb, 2), PushResult::Duplicate);
        assert_eq!(jb.pop(), pkt(0));
        assert_eq!(jb.pop(), pkt(1));
        assert_eq!(push(&mut jb, 1), PushResult::TooLate);
        assert_eq!(push(&mut jb, 0), PushResult::TooLate);
        assert_eq!(jb.pop(), pkt(2));
        assert_eq!(jb.pop(), pkt(3));
        let st = jb.stats();
        assert_eq!((st.received, st.late, st.duplicate, st.lost), (4, 2, 1, 0));
    }

    #[test]
    fn startup_reordering_before_first_pop() {
        let mut jb = JitterBuffer::new(fixed_cfg());
        assert_eq!(push(&mut jb, 5), PushResult::Accepted);
        assert_eq!(push(&mut jb, 3), PushResult::Accepted);
        assert_eq!(push(&mut jb, 4), PushResult::Accepted);
        assert_eq!(jb.pop(), pkt(3));
        assert_eq!(jb.pop(), pkt(4));
        assert_eq!(jb.pop(), pkt(5));
    }

    #[test]
    fn loss_reports_missing_with_next_for_fec() {
        let mut jb = JitterBuffer::new(fixed_cfg());
        for s in [0, 1, 2, 4, 7] {
            push(&mut jb, s);
        }
        assert_eq!(jb.pop(), pkt(0));
        assert_eq!(jb.pop(), pkt(1));
        assert_eq!(jb.pop(), pkt(2));
        assert_eq!(
            jb.pop(),
            Pop::Missing {
                next: Some(4_u32.to_be_bytes().to_vec())
            }
        );
        // The FEC source stays buffered.
        assert_eq!(jb.pop(), pkt(4));
        assert_eq!(jb.pop(), Pop::Missing { next: None });
        assert_eq!(
            jb.pop(),
            Pop::Missing {
                next: Some(7_u32.to_be_bytes().to_vec())
            }
        );
        // A lost packet arriving after its slot was concealed is too late.
        assert_eq!(push(&mut jb, 5), PushResult::TooLate);
        assert_eq!(jb.pop(), pkt(7));
        assert_eq!(jb.stats().lost, 3);
    }

    #[test]
    fn leading_gap_is_skipped_when_priming() {
        let mut jb = JitterBuffer::new(fixed_cfg());
        push(&mut jb, 10);
        push(&mut jb, 13); // 11, 12 missing
                           // The first pop anchors the window: 11/12 now arrive... never; prime on 3 packets.
        push(&mut jb, 14);
        assert!(jb.is_primed());
        assert_eq!(jb.pop(), pkt(10));
        assert!(matches!(jb.pop(), Pop::Missing { .. }));
        assert!(matches!(jb.pop(), Pop::Missing { .. }));
        assert_eq!(jb.pop(), pkt(13));
        assert_eq!(jb.pop(), pkt(14));
        assert_eq!(jb.pop(), Pop::Underrun);
        // After the underrun, packets 15..=16 are lost; playout restarts on 17 without
        // conceal slots for 15/16.
        for s in [17, 18, 19] {
            push(&mut jb, s);
        }
        assert_eq!(jb.pop(), pkt(17));
        assert_eq!(jb.stats().lost, 4);
    }

    #[test]
    fn sequence_wraparound() {
        let mut jb = JitterBuffer::new(fixed_cfg());
        let start = u32::MAX - 2;
        let order = [start, start + 2, start + 1, 0, 2, 1, 3];
        for s in order {
            assert_eq!(push(&mut jb, s), PushResult::Accepted, "seq {s}");
        }
        let mut popped = Vec::new();
        for _ in 0..7 {
            match jb.pop() {
                Pop::Packet(p) => popped.push(u32::from_be_bytes([p[0], p[1], p[2], p[3]])),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert_eq!(popped, vec![start, start + 1, start + 2, 0, 1, 2, 3]);
        // Wrap-aware lateness and duplicates.
        assert_eq!(push(&mut jb, u32::MAX), PushResult::TooLate);
        assert_eq!(push(&mut jb, 3), PushResult::TooLate);
        assert_eq!(push(&mut jb, 5), PushResult::Accepted);
        assert_eq!(push(&mut jb, 5), PushResult::Duplicate);
    }

    #[test]
    fn overflow_drops_oldest() {
        let mut jb = JitterBuffer::new(JitterConfig {
            capacity: 4,
            ..fixed_cfg()
        });
        for s in 0..4 {
            assert_eq!(push(&mut jb, s), PushResult::Accepted);
        }
        assert_eq!(push(&mut jb, 4), PushResult::Overflow);
        assert_eq!(jb.buffered_ms(), 40.0);
        assert_eq!(jb.pop(), pkt(1));
        assert_eq!(push(&mut jb, 0), PushResult::TooLate);
        assert_eq!(jb.stats().lost, 1);
        // Span limit: seq 5 would need a 5-slot window (1..=5) with capacity 4.
        let mut jb = JitterBuffer::new(JitterConfig {
            capacity: 4,
            ..fixed_cfg()
        });
        for s in [0, 1, 2, 3] {
            push(&mut jb, s);
        }
        assert_eq!(jb.pop(), pkt(0));
        assert_eq!(push(&mut jb, 5), PushResult::Overflow); // drops 1
        assert_eq!(push(&mut jb, 4), PushResult::Accepted);
        assert_eq!(jb.pop(), pkt(2));
        assert_eq!(jb.pop(), pkt(3));
        assert_eq!(jb.pop(), pkt(4));
        assert_eq!(jb.pop(), pkt(5));
        assert_eq!(jb.stats().lost, 1);
        // Before playout, a packet too far behind the window is dropped.
        let mut jb = JitterBuffer::new(JitterConfig {
            capacity: 4,
            ..fixed_cfg()
        });
        push(&mut jb, 10);
        push(&mut jb, 11);
        assert_eq!(push(&mut jb, 7), PushResult::Overflow);
        assert_eq!(push(&mut jb, 8), PushResult::Accepted);
    }

    #[test]
    fn jump_ahead_moves_window_without_long_concealment() {
        let mut jb = JitterBuffer::new(fixed_cfg());
        for s in 0..4 {
            push(&mut jb, s);
        }
        assert_eq!(jb.pop(), pkt(0));
        assert_eq!(push(&mut jb, 1_000), PushResult::Overflow);
        assert_eq!(jb.buffered_ms(), 10.0);
        push(&mut jb, 1_001);
        push(&mut jb, 1_002);
        assert_eq!(jb.pop(), pkt(1_000));
        // 1, 2, 3 were dropped and 4..=999 never arrived.
        assert_eq!(jb.stats().lost, 999);
        assert_eq!(push(&mut jb, 999), PushResult::TooLate);
    }

    #[test]
    fn adaptive_target_grows_fast_and_decays_slowly() {
        let mut jb = JitterBuffer::new(JitterConfig::default());
        assert_eq!(jb.target_ms(), 40.0);
        // Alternating 0 / 20 ms extra delay: |D| = 20 ms on every packet.
        let mut seq = 0_u32;
        let mut feed = |jb: &mut JitterBuffer, n: u32, extra: &dyn Fn(u32) -> u64| {
            for _ in 0..n {
                let arrival = u64::from(seq) * 10_000 + extra(seq);
                jb.push(seq, seq.wrapping_mul(480), arrival, vec![0]);
                let _ = jb.pop();
                seq += 1;
            }
        };
        feed(&mut jb, 20, &|s| if s % 2 == 0 { 0 } else { 20_000 });
        let after_20 = jb.target_ms();
        assert!(after_20 > 50.0, "target rises within 200 ms: {after_20}");
        feed(&mut jb, 200, &|s| if s % 2 == 0 { 0 } else { 20_000 });
        let j = f64::from(jb.stats().jitter_ms);
        assert!((j - 20.0).abs() < 0.5, "jitter estimate {j}");
        let peak = jb.target_ms();
        assert!((peak - 70.0).abs() < 2.0, "target = 10 + 3·J: {peak}");
        // Perfect timing again: J collapses quickly but the target decays slowly.
        feed(&mut jb, 100, &|_| 0);
        assert!(jb.stats().jitter_ms < 0.1);
        let after_1s = jb.target_ms();
        assert!(after_1s < peak && after_1s > 55.0, "slow decay: {after_1s}");
        feed(&mut jb, 6_000, &|_| 0);
        assert!(
            (jb.target_ms() - 20.0).abs() < 0.5,
            "back to min: {}",
            jb.target_ms()
        );
        // Upper clamp.
        feed(&mut jb, 300, &|s| if s % 2 == 0 { 0 } else { 200_000 });
        assert_eq!(jb.target_ms(), 150.0);
    }

    #[test]
    fn reset_keeps_stats_and_restarts_stream() {
        let mut jb = JitterBuffer::new(fixed_cfg());
        for s in 0..4 {
            push(&mut jb, s);
        }
        assert_eq!(jb.pop(), pkt(0));
        jb.reset();
        assert!(!jb.is_primed());
        assert_eq!(jb.buffered_ms(), 0.0);
        assert_eq!(jb.pop(), Pop::Underrun);
        // After a reset any seq starts a new window (even an old one).
        for s in 0..3 {
            assert_eq!(push(&mut jb, s), PushResult::Accepted);
        }
        assert_eq!(jb.pop(), pkt(0));
        assert_eq!(jb.stats().received, 7);
    }
}
