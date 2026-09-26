//! Level metering.

use serde::{Deserialize, Serialize};

/// Floor used for silence, in dBFS.
pub const SILENCE_DB: f32 = -120.0;

/// Peak and RMS level of a block, in dBFS (floored at [`SILENCE_DB`]).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Level {
    /// Peak absolute sample value in dBFS.
    pub peak_db: f32,
    /// RMS level in dBFS.
    pub rms_db: f32,
}

impl Level {
    /// Digital silence.
    pub const SILENT: Level = Level {
        peak_db: SILENCE_DB,
        rms_db: SILENCE_DB,
    };
}

impl Default for Level {
    fn default() -> Self {
        Self::SILENT
    }
}

/// Converts a linear amplitude to dBFS, floored at [`SILENCE_DB`].
///
/// Zero and NaN map to [`SILENCE_DB`], an infinite amplitude to `f32::MAX` (so the result is
/// always finite and serializable); negative amplitudes are treated by magnitude.
pub fn amplitude_to_db(amplitude: f32) -> f32 {
    let a = amplitude.abs();
    if !a.is_finite() {
        return if a.is_nan() { SILENCE_DB } else { f32::MAX };
    }
    if a <= 0.0 {
        return SILENCE_DB;
    }
    (20.0 * a.log10()).max(SILENCE_DB)
}

/// Converts dB to a linear gain factor.
///
/// Values at or below [`SILENCE_DB`] map to exactly `0.0`.
pub fn db_to_amplitude(db: f32) -> f32 {
    if db <= SILENCE_DB {
        return 0.0;
    }
    10.0_f32.powf(db / 20.0)
}

/// Computes the [`Level`] of a block from its peak magnitude and sum of squares over `n`
/// samples. Shared by [`LevelMeter`] and the mixer.
pub(crate) fn level_from_stats(peak: f32, sum_sq: f64, n: usize) -> Level {
    if n == 0 {
        return Level::SILENT;
    }
    let rms = (sum_sq / n as f64).sqrt() as f32;
    Level {
        peak_db: amplitude_to_db(peak),
        rms_db: amplitude_to_db(rms),
    }
}

/// Measures blocks of interleaved samples.
#[derive(Debug, Clone, Default)]
pub struct LevelMeter {
    last: Level,
}

impl LevelMeter {
    /// Creates a meter reporting [`Level::SILENT`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Measures one block (all channels together), stores and returns its level.
    ///
    /// An empty block measures as [`Level::SILENT`]. Non-finite samples are ignored (counted
    /// as silence) so one corrupt sample cannot poison the meter.
    pub fn process(&mut self, interleaved: &[f32]) -> Level {
        let mut peak = 0.0_f32;
        let mut sum_sq = 0.0_f64;
        for &x in interleaved {
            if x.is_finite() {
                let a = x.abs();
                peak = peak.max(a);
                sum_sq += f64::from(x) * f64::from(x);
            }
        }
        self.last = level_from_stats(peak, sum_sq, interleaved.len());
        self.last
    }

    /// Level of the last processed block.
    pub fn level(&self) -> Level {
        self.last
    }

    /// Resets to [`Level::SILENT`].
    pub fn reset(&mut self) {
        self.last = Level::SILENT;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::AudioFormat;
    use crate::tone::SineGenerator;

    fn close(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    #[test]
    fn db_conversions() {
        assert!(close(amplitude_to_db(1.0), 0.0, 1e-6));
        assert!(close(amplitude_to_db(0.5), -6.0206, 1e-3));
        assert!(close(amplitude_to_db(-0.1), -20.0, 1e-4));
        assert_eq!(amplitude_to_db(0.0), SILENCE_DB);
        assert_eq!(amplitude_to_db(1e-9), SILENCE_DB);
        assert_eq!(amplitude_to_db(f32::NAN), SILENCE_DB);
        assert!(close(db_to_amplitude(-6.0206), 0.5, 1e-4));
        assert!(close(db_to_amplitude(0.0), 1.0, 1e-6));
        assert_eq!(db_to_amplitude(SILENCE_DB), 0.0);
        for db in [-60.0_f32, -12.0, -3.0, 6.0] {
            assert!(close(amplitude_to_db(db_to_amplitude(db)), db, 1e-3));
        }
    }

    #[test]
    fn meter_measures_sine_peak_and_rms() {
        let mut gen = SineGenerator::new(1000.0, 0.5, AudioFormat::INTERNAL);
        let mut buf = vec![0.0; 4800 * 2];
        gen.fill(&mut buf);
        let mut m = LevelMeter::new();
        let level = m.process(&buf);
        // Peak 0.5 = -6.02 dBFS, RMS of a sine = peak / sqrt(2) = -9.03 dBFS.
        assert!(close(level.peak_db, -6.02, 0.05), "{level:?}");
        assert!(close(level.rms_db, -9.03, 0.05), "{level:?}");
        assert_eq!(m.level(), level);
        m.reset();
        assert_eq!(m.level(), Level::SILENT);
    }

    #[test]
    fn meter_handles_silence_empty_and_nan() {
        let mut m = LevelMeter::new();
        assert_eq!(m.process(&[]), Level::SILENT);
        assert_eq!(m.process(&[0.0; 64]), Level::SILENT);
        let l = m.process(&[f32::NAN, 1.0, -1.0, f32::INFINITY]);
        assert!(close(l.peak_db, 0.0, 1e-6));
        // Two unit samples out of four: RMS = sqrt(2/4) = -3.01 dBFS.
        assert!(close(l.rms_db, -3.01, 0.01), "{l:?}");
    }
}
