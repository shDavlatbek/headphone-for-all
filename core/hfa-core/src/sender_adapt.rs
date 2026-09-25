//! Sender-side adaptation to the loss the hub reports in `Stats` (pure logic, no I/O).
//!
//! - **Redundancy** (a copy of the previous Opus packet in every datagram, see
//!   [`crate::payload`]): switched on as soon as the reported loss exceeds
//!   [`REDUNDANCY_ON_LOSS_PCT`], switched off once the loss stayed below
//!   [`REDUNDANCY_OFF_LOSS_PCT`] for [`REDUNDANCY_OFF_AFTER`].
//! - **Expected loss** for the Opus encoder (in-band FEC, useful for speech-like input): the
//!   reported loss rounded, capped at [`MAX_EXPECTED_LOSS_PCT`].
//! - **Bitrate**: above [`BITRATE_DOWN_LOSS_PCT`] loss in [`HEAVY_LOSS_REPORTS`] consecutive
//!   reports (congestion, not a random burst) the bitrate drops by 25 % per report, not below [`MIN_ADAPTIVE_BITRATE`] (or the configured bitrate if that is
//!   lower); after [`RECOVER_AFTER`] below [`RECOVER_LOSS_PCT`] it climbs back by 10 % per
//!   step, up to the configured bitrate.
//!
//! The loss the hub reports is the **network** loss (before redundancy/FEC recovery), so
//! enabling redundancy does not make the reported loss vanish and switch it off again.

use std::time::{Duration, Instant};

/// Loss (percent) above which redundancy is enabled.
pub(crate) const REDUNDANCY_ON_LOSS_PCT: f32 = 2.0;
/// Loss (percent) below which redundancy may be disabled again.
pub(crate) const REDUNDANCY_OFF_LOSS_PCT: f32 = 0.5;
/// How long the loss must stay below [`REDUNDANCY_OFF_LOSS_PCT`] to disable redundancy.
pub(crate) const REDUNDANCY_OFF_AFTER: Duration = Duration::from_secs(10);
/// Loss (percent) above which the bitrate is reduced.
pub(crate) const BITRATE_DOWN_LOSS_PCT: f32 = 10.0;
/// Consecutive reports above [`BITRATE_DOWN_LOSS_PCT`] before the bitrate is reduced.
pub(crate) const HEAVY_LOSS_REPORTS: u32 = 2;
/// Lowest bitrate the adaptation goes down to (unless the configured one is lower).
pub(crate) const MIN_ADAPTIVE_BITRATE: u32 = 48_000;
/// Loss (percent) below which the bitrate may recover.
pub(crate) const RECOVER_LOSS_PCT: f32 = 1.0;
/// Time below [`RECOVER_LOSS_PCT`] before each recovery step.
pub(crate) const RECOVER_AFTER: Duration = Duration::from_secs(10);
/// Largest expected-loss hint given to the Opus encoder.
pub(crate) const MAX_EXPECTED_LOSS_PCT: u8 = 30;

/// What the encoder should use after a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Adaptation {
    /// Send a redundant copy of the previous frame.
    pub redundancy: bool,
    /// Opus bitrate in bits per second.
    pub bitrate: u32,
    /// Expected loss hint for the Opus encoder (percent).
    pub expected_loss: u8,
}

/// The adaptation state machine of one sender.
#[derive(Debug, Clone)]
pub(crate) struct Adapter {
    base_bitrate: u32,
    current: Adaptation,
    /// Since when the loss has been below [`REDUNDANCY_OFF_LOSS_PCT`].
    quiet_since: Option<Instant>,
    /// Since when (or since the last recovery step) the loss has been below
    /// [`RECOVER_LOSS_PCT`].
    good_since: Option<Instant>,
    /// Consecutive reports above [`BITRATE_DOWN_LOSS_PCT`].
    heavy_reports: u32,
}

impl Adapter {
    /// Starts at the configured bitrate, without redundancy, with `expected_loss`.
    pub(crate) fn new(base_bitrate: u32, expected_loss: u8) -> Self {
        Self {
            base_bitrate,
            current: Adaptation {
                redundancy: false,
                bitrate: base_bitrate,
                expected_loss,
            },
            quiet_since: None,
            good_since: None,
            heavy_reports: 0,
        }
    }

    /// The current decision.
    pub(crate) fn current(&self) -> Adaptation {
        self.current
    }

    fn floor(&self) -> u32 {
        MIN_ADAPTIVE_BITRATE.min(self.base_bitrate)
    }

    /// Feeds one loss report (percent, over the last stats interval) received at `now`, and
    /// returns the new decision.
    pub(crate) fn on_report(&mut self, loss_pct: f32, now: Instant) -> Adaptation {
        let loss = if loss_pct.is_finite() {
            loss_pct.clamp(0.0, 100.0)
        } else {
            0.0
        };

        // Redundancy.
        if loss > REDUNDANCY_ON_LOSS_PCT {
            self.current.redundancy = true;
        }
        if loss < REDUNDANCY_OFF_LOSS_PCT {
            let since = *self.quiet_since.get_or_insert(now);
            if self.current.redundancy && now.duration_since(since) >= REDUNDANCY_OFF_AFTER {
                self.current.redundancy = false;
            }
        } else {
            self.quiet_since = None;
        }

        // Expected loss hint (rounded, capped).
        self.current.expected_loss = (loss.round() as u8).min(MAX_EXPECTED_LOSS_PCT);

        // Bitrate.
        if loss > BITRATE_DOWN_LOSS_PCT {
            self.heavy_reports = self.heavy_reports.saturating_add(1);
        } else {
            self.heavy_reports = 0;
        }
        if self.heavy_reports >= HEAVY_LOSS_REPORTS {
            let reduced = (u64::from(self.current.bitrate) * 3 / 4) as u32;
            self.current.bitrate = reduced.max(self.floor()).min(self.current.bitrate);
            self.good_since = None;
        } else if loss > BITRATE_DOWN_LOSS_PCT {
            self.good_since = None;
        } else if loss < RECOVER_LOSS_PCT {
            let since = *self.good_since.get_or_insert(now);
            if self.current.bitrate < self.base_bitrate
                && now.duration_since(since) >= RECOVER_AFTER
            {
                let raised = (u64::from(self.current.bitrate) * 11 / 10) as u32;
                self.current.bitrate = raised.clamp(self.current.bitrate + 1, self.base_bitrate);
                self.good_since = Some(now);
            }
        } else {
            self.good_since = None;
        }
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(t0: Instant, s: u64) -> Instant {
        t0 + Duration::from_secs(s)
    }

    #[test]
    fn redundancy_turns_on_above_2_pct_and_off_after_10_quiet_seconds() {
        let t0 = Instant::now();
        let mut a = Adapter::new(128_000, 5);
        assert!(!a.on_report(1.5, at(t0, 0)).redundancy);
        assert!(a.on_report(2.5, at(t0, 1)).redundancy);
        // Moderate loss keeps it on.
        assert!(a.on_report(1.0, at(t0, 2)).redundancy);
        // Quiet for less than 10 s: still on.
        for s in 3..13 {
            assert!(a.on_report(0.1, at(t0, s)).redundancy, "second {s}");
        }
        // 10 s after the first quiet report.
        assert!(!a.on_report(0.0, at(t0, 13)).redundancy);
    }

    #[test]
    fn a_loss_burst_restarts_the_quiet_timer() {
        let t0 = Instant::now();
        let mut a = Adapter::new(128_000, 5);
        a.on_report(5.0, at(t0, 0));
        a.on_report(0.0, at(t0, 1));
        a.on_report(0.8, at(t0, 8)); // not quiet: restarts
        assert!(a.on_report(0.0, at(t0, 12)).redundancy);
        assert!(!a.on_report(0.0, at(t0, 22)).redundancy);
    }

    #[test]
    fn heavy_loss_reduces_bitrate_down_to_the_floor() {
        let t0 = Instant::now();
        let mut a = Adapter::new(128_000, 5);
        // One bad second is a burst, not congestion.
        assert_eq!(a.on_report(12.0, at(t0, 0)).bitrate, 128_000);
        assert_eq!(a.on_report(3.0, at(t0, 1)).bitrate, 128_000);
        assert_eq!(a.on_report(12.0, at(t0, 2)).bitrate, 128_000);
        assert_eq!(a.on_report(12.0, at(t0, 3)).bitrate, 96_000);
        assert_eq!(a.on_report(12.0, at(t0, 4)).bitrate, 72_000);
        assert_eq!(a.on_report(12.0, at(t0, 5)).bitrate, 54_000);
        assert_eq!(a.on_report(12.0, at(t0, 6)).bitrate, 48_000);
        assert_eq!(a.on_report(50.0, at(t0, 7)).bitrate, 48_000);
        assert_eq!(a.current().expected_loss, MAX_EXPECTED_LOSS_PCT);
    }

    #[test]
    fn low_configured_bitrate_is_never_raised_or_cut_below_itself() {
        let t0 = Instant::now();
        let mut a = Adapter::new(32_000, 0);
        assert_eq!(a.on_report(20.0, at(t0, 0)).bitrate, 32_000);
        assert_eq!(a.on_report(20.0, at(t0, 0)).bitrate, 32_000);
        for s in 1..40 {
            assert_eq!(a.on_report(0.0, at(t0, s)).bitrate, 32_000);
        }
    }

    #[test]
    fn bitrate_recovers_slowly_after_10_good_seconds() {
        let t0 = Instant::now();
        let mut a = Adapter::new(128_000, 5);
        a.on_report(15.0, at(t0, 0));
        a.on_report(15.0, at(t0, 1));
        a.on_report(15.0, at(t0, 1));
        assert_eq!(a.current().bitrate, 72_000);
        // 1..10 % loss: neither down nor up.
        assert_eq!(a.on_report(5.0, at(t0, 2)).bitrate, 72_000);
        for s in 3..13 {
            assert_eq!(a.on_report(0.2, at(t0, s)).bitrate, 72_000, "second {s}");
        }
        assert_eq!(a.on_report(0.2, at(t0, 13)).bitrate, 79_200);
        // The next step needs another 10 s.
        assert_eq!(a.on_report(0.2, at(t0, 14)).bitrate, 79_200);
        let mut s = 14;
        while a.current().bitrate < 128_000 {
            s += 1;
            a.on_report(0.0, at(t0, s));
            assert!(s < 200, "never recovered");
        }
        assert_eq!(a.current().bitrate, 128_000);
    }

    #[test]
    fn non_finite_reports_count_as_no_loss() {
        let mut a = Adapter::new(128_000, 5);
        let r = a.on_report(f32::NAN, Instant::now());
        assert_eq!(r.expected_loss, 0);
        assert!(!r.redundancy);
        assert_eq!(r.bitrate, 128_000);
    }
}
