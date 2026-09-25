//! Clock-drift compensation: a PI controller that keeps a jitter buffer at its target level
//! by nudging the resample ratio.
//!
//! Sign convention: the returned value is the **relative resample ratio** (output rate /
//! input rate multiplier, as used by [`crate::StreamResampler::set_ratio_relative`]). When the
//! buffer is fuller than the target, the ratio is < 1 (the stream is consumed slightly
//! faster); when it is emptier, the ratio is > 1.
//!
//! # Control law
//!
//! With the error `e = buffered_ms − target_ms`, first low-pass filtered (one pole, time
//! constant [`MEASUREMENT_TAU_S`]) to average out packet quantization and arrival jitter:
//!
//! ```text
//! u     = kp·e_f + ki·∫e_f dt          (u > 0 ⇔ buffer too full)
//! ratio = 1 − clamp(u, ±max_ppm·1e-6)
//! ```
//!
//! Anti-windup: the integral term alone is clamped to `±max_ppm·1e-6`, and the integrator
//! is frozen while the output is saturated in the direction the error pushes it.
//!
//! Tuning: the buffer level obeys `de/dt ≈ 1000·(δ − (1 − ratio))` ms/s for a sender clock
//! offset `δ`, so the loop is second order with `ωn = √(1000·ki)` and
//! `ζ = 1000·kp / (2·ωn)`. The defaults (`kp = 5e-5`, `ki = 1e-6`) give `ωn ≈ 0.032 rad/s`
//! and `ζ ≈ 0.8`: a ±300 ppm offset is absorbed with a transient of a few ms of buffer
//! error and zero steady-state error.

use serde::{Deserialize, Serialize};

/// Time constant of the low-pass filter applied to the buffer-level error, in seconds.
pub const MEASUREMENT_TAU_S: f64 = 2.0;

/// PI controller parameters.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DriftConfig {
    /// Proportional gain (ratio change per ms of error).
    pub kp: f64,
    /// Integral gain (ratio change per ms·s of accumulated error).
    pub ki: f64,
    /// Maximum deviation from 1.0 in parts per million.
    pub max_ppm: f64,
}

impl Default for DriftConfig {
    /// `kp = 5e-5`, `ki = 1e-6`, `max_ppm = 2000` (±0.2 %, inaudible). See the module docs
    /// for the tuning rationale.
    fn default() -> Self {
        Self {
            kp: 5.0e-5,
            ki: 1.0e-6,
            max_ppm: 2000.0,
        }
    }
}

/// The PI drift controller.
#[derive(Debug, Clone)]
pub struct DriftController {
    config: DriftConfig,
    /// Integral of the filtered error, in ms·s.
    integral: f64,
    /// Filtered error in ms; `None` before the first measurement.
    filtered: Option<f64>,
    ratio: f64,
}

impl DriftController {
    /// Creates a controller at ratio 1.0.
    pub fn new(config: DriftConfig) -> Self {
        Self {
            config,
            integral: 0.0,
            filtered: None,
            ratio: 1.0,
        }
    }

    /// Maximum deviation of the ratio from 1.0 (a fraction, not ppm).
    fn max_dev(&self) -> f64 {
        let m = self.config.max_ppm * 1e-6;
        if m.is_finite() {
            m.max(0.0)
        } else {
            0.0
        }
    }

    /// The configuration.
    pub fn config(&self) -> &DriftConfig {
        &self.config
    }

    /// Feeds one measurement (`dt_s` seconds since the previous one) and returns the new
    /// relative ratio, clamped to `1 ± max_ppm·1e-6`.
    ///
    /// Non-finite inputs or a non-positive `dt_s` leave the state untouched and return the
    /// previous ratio.
    pub fn update(&mut self, buffered_ms: f64, target_ms: f64, dt_s: f64) -> f64 {
        let e = buffered_ms - target_ms;
        if !e.is_finite() || !dt_s.is_finite() || dt_s <= 0.0 {
            return self.ratio;
        }
        let ef = match self.filtered {
            None => e,
            Some(prev) => prev + (e - prev) * (dt_s / (MEASUREMENT_TAU_S + dt_s)),
        };
        self.filtered = Some(ef);
        let max_dev = self.max_dev();
        let ki = self.config.ki;
        let kp = self.config.kp;

        // Candidate integral, with the integral term alone limited to the output range.
        let mut integral = self.integral + ef * dt_s;
        if ki > 0.0 {
            let lim = max_dev / ki;
            integral = integral.clamp(-lim, lim);
        } else {
            integral = 0.0;
        }
        let u = kp * ef + ki * integral;
        // Conditional integration: do not wind up further while saturated in the error's
        // direction.
        let saturated = u.abs() > max_dev && u.signum() == ef.signum();
        if !saturated || integral.abs() < self.integral.abs() {
            self.integral = integral;
        }
        let u = (kp * ef + ki * self.integral).clamp(-max_dev, max_dev);
        self.ratio = if u.is_finite() { 1.0 - u } else { 1.0 };
        self.ratio
    }

    /// The most recently returned ratio.
    pub fn ratio(&self) -> f64 {
        self.ratio
    }

    /// The deviation of the current ratio from 1.0 in ppm (negative = consuming faster).
    pub fn ppm(&self) -> f64 {
        (self.ratio - 1.0) * 1e6
    }

    /// Resets the integrator, the measurement filter and the ratio to 1.0.
    pub fn reset(&mut self) {
        self.integral = 0.0;
        self.filtered = None;
        self.ratio = 1.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_convention_and_clamp() {
        let mut c = DriftController::new(DriftConfig::default());
        assert_eq!(c.ratio(), 1.0);
        let r = c.update(80.0, 40.0, 0.01);
        assert!(r < 1.0, "too full -> consume faster: {r}");
        c.reset();
        let r = c.update(0.0, 40.0, 0.01);
        assert!(r > 1.0, "too empty -> consume slower: {r}");
        // A huge persistent error saturates at max_ppm.
        c.reset();
        for _ in 0..100_000 {
            c.update(1_000.0, 40.0, 0.01);
        }
        assert!((c.ratio() - (1.0 - 2000e-6)).abs() < 1e-12);
        assert!((c.ppm() + 2000.0).abs() < 1e-6);
        c.reset();
        assert_eq!(c.ratio(), 1.0);
    }

    #[test]
    fn anti_windup_recovers_quickly() {
        let mut c = DriftController::new(DriftConfig::default());
        // Saturate for a long time...
        for _ in 0..60_000 {
            c.update(500.0, 40.0, 0.01);
        }
        // ...then flip the error: the ratio must leave the lower rail within seconds, not
        // after unwinding ten minutes of integral.
        let mut t = 0.0;
        while c.update(0.0, 40.0, 0.01) < 1.0 {
            t += 0.01;
            assert!(t < 30.0, "integrator wound up");
        }
    }

    #[test]
    fn ignores_invalid_input() {
        let mut c = DriftController::new(DriftConfig::default());
        let r = c.update(50.0, 40.0, 0.01);
        assert_eq!(c.update(f64::NAN, 40.0, 0.01), r);
        assert_eq!(c.update(50.0, 40.0, 0.0), r);
        assert_eq!(c.update(50.0, 40.0, -1.0), r);
        assert_eq!(c.config(), &DriftConfig::default());
    }

    /// Continuous-time plant: the buffer integrates the rate mismatch.
    #[test]
    fn converges_on_ideal_plant() {
        for ppm in [-300.0_f64, 300.0, 1500.0] {
            let mut c = DriftController::new(DriftConfig::default());
            let (target, dt) = (40.0, 0.01);
            let mut level = target;
            let mut worst = 0.0_f64;
            for step in 0..60_000 {
                let ratio = c.update(level, target, dt);
                level += 1000.0 * (ppm * 1e-6 - (1.0 - ratio)) * dt;
                worst = worst.max((level - target).abs());
                if step > 30_000 {
                    assert!(
                        (level - target).abs() < 0.5,
                        "{ppm} ppm: error {}",
                        level - target
                    );
                }
            }
            // Steady state: the ratio cancels the clock offset exactly.
            assert!((c.ppm() + ppm).abs() < 5.0, "{ppm}: ratio {} ppm", c.ppm());
            assert!(
                worst < 10.0 * ppm.abs() / 300.0,
                "{ppm}: transient {worst} ms"
            );
        }
    }
}
