//! Sliding anti-replay window for media sequence numbers (RFC 6479 / WireGuard style).
//!
//! The window remembers the highest accepted `seq` and a bitmap of the
//! [`REPLAY_WINDOW`] sequence numbers at and below it. A `seq` is fresh if it is above the
//! highest one, or inside the window and not seen yet. Because `seq` never wraps or resets
//! for a key (see [`crate::media::MediaHeader::seq`]), plain integer comparisons suffice.

/// Number of sequence numbers tracked below the highest accepted one (1.28 s of 10 ms
/// packets, far beyond the jitter buffer's maximum depth).
pub const REPLAY_WINDOW: u32 = 128;

/// Replay state of one stream. Not reset by `FLAG_RESET`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplayWindow {
    /// Highest accepted `seq`, `None` before the first packet.
    highest: Option<u32>,
    /// Bit `i` set = `highest - i` was accepted.
    seen: u128,
}

impl ReplayWindow {
    /// An empty window (every `seq` is fresh).
    pub fn new() -> Self {
        Self::default()
    }

    /// `true` if `seq` has not been accepted yet and is not older than the window. Does not
    /// change the window; call it before the (more expensive) authentication.
    pub fn check(&self, seq: u32) -> bool {
        match self.highest {
            None => true,
            Some(high) if seq > high => true,
            Some(high) => {
                let age = high - seq;
                age < REPLAY_WINDOW && self.seen & (1u128 << age) == 0
            }
        }
    }

    /// Records `seq` as received. Call it only after the datagram was authenticated. Returns
    /// `false` (and changes nothing) if `seq` is a replay or too old.
    pub fn accept(&mut self, seq: u32) -> bool {
        match self.highest {
            None => {
                self.highest = Some(seq);
                self.seen = 1;
                true
            }
            Some(high) if seq > high => {
                let shift = seq - high;
                self.seen = if shift >= REPLAY_WINDOW {
                    0
                } else {
                    self.seen << shift
                };
                self.seen |= 1;
                self.highest = Some(seq);
                true
            }
            Some(high) => {
                let age = high - seq;
                if age >= REPLAY_WINDOW {
                    return false;
                }
                let bit = 1u128 << age;
                if self.seen & bit != 0 {
                    return false;
                }
                self.seen |= bit;
                true
            }
        }
    }

    /// Highest accepted `seq`, if any.
    pub fn highest(&self) -> Option<u32> {
        self.highest
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn in_order_and_duplicates() {
        let mut w = ReplayWindow::new();
        assert!(w.check(0));
        for seq in 0..500 {
            assert!(w.check(seq));
            assert!(w.accept(seq));
            assert!(!w.check(seq), "seq {seq} accepted twice");
            assert!(!w.accept(seq));
        }
        assert_eq!(w.highest(), Some(499));
    }

    #[test]
    fn out_of_order_inside_window_is_accepted_once() {
        let mut w = ReplayWindow::new();
        assert!(w.accept(10));
        assert!(w.accept(12));
        assert!(w.accept(11), "late but inside the window");
        assert!(!w.accept(11));
        assert!(!w.accept(12));
        assert!(w.accept(5), "never seen, inside the window");
        assert_eq!(w.highest(), Some(12));
    }

    #[test]
    fn too_old_is_rejected() {
        let mut w = ReplayWindow::new();
        assert!(w.accept(1000));
        let oldest_ok = 1000 - (REPLAY_WINDOW - 1);
        assert!(w.check(oldest_ok));
        assert!(!w.check(oldest_ok - 1));
        assert!(!w.accept(oldest_ok - 1));
        assert!(!w.accept(0));
        // A rejected packet does not change the state.
        assert_eq!(w.highest(), Some(1000));
    }

    #[test]
    fn large_jump_clears_history() {
        let mut w = ReplayWindow::new();
        for seq in 0..10 {
            assert!(w.accept(seq));
        }
        assert!(w.accept(10_000));
        assert!(!w.check(9), "far behind the new highest");
        assert!(w.accept(10_000 - 5));
        assert!(!w.accept(10_000));
    }

    #[test]
    fn works_at_the_top_of_the_sequence_space() {
        let mut w = ReplayWindow::new();
        assert!(w.accept(u32::MAX - 1));
        assert!(w.accept(u32::MAX));
        assert!(!w.accept(u32::MAX));
        assert!(w.accept(u32::MAX - 2));
        assert!(!w.accept(u32::MAX - 1));
    }

    #[test]
    fn check_does_not_record() {
        let mut w = ReplayWindow::new();
        assert!(w.check(3));
        assert!(w.check(3));
        assert_eq!(w.highest(), None);
        assert!(w.accept(3));
    }

    proptest! {
        /// Against a reference model: a seq is accepted iff it was never accepted before and
        /// is within REPLAY_WINDOW of the highest accepted seq.
        #[test]
        fn matches_reference_model(seqs in proptest::collection::vec(0u32..400, 1..300)) {
            let mut w = ReplayWindow::new();
            let mut accepted = std::collections::HashSet::new();
            let mut high: Option<u32> = None;
            for seq in seqs {
                let fresh = !accepted.contains(&seq)
                    && high.is_none_or(|h| seq > h || h - seq < REPLAY_WINDOW);
                prop_assert_eq!(w.check(seq), fresh);
                prop_assert_eq!(w.accept(seq), fresh);
                if fresh {
                    accepted.insert(seq);
                    high = Some(high.map_or(seq, |h| h.max(seq)));
                }
            }
        }
    }
}
