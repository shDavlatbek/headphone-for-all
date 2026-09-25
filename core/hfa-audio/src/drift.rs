//! Clock-drift compensation: a PI controller that keeps a jitter buffer at its target level
//! by nudging the resample ratio.
//!
//! Sign convention: the returned value is the **relative resample ratio** (output rate /
//! input rate multiplier, as used by [`crate::StreamResampler::set_ratio_relative`]). When the
//! buffer is fuller than the target, the ratio is < 1 (the stream is consumed slightly
//! faster); when it is emptier, the ratio is > 1.

use serde::{Deserialize, Serialize};

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
    /// `max_ppm = 2000` (±0.2 %, inaudible). Gains are starting values; `feat/audio` tunes them.
    fn default() -> Self {
        Self {
            kp: 1.0e-5,
            ki: 1.0e-6,
            max_ppm: 2000.0,
        }
    }
}

/// The PI drift controller.
#[derive(Debug, Clone)]
pub struct DriftController {
    config: DriftConfig,
}

impl DriftController {
    /// Creates a controller at ratio 1.0.
    pub fn new(config: DriftConfig) -> Self {
        Self { config }
    }

    /// The configuration.
    pub fn config(&self) -> &DriftConfig {
        &self.config
    }

    /// Feeds one measurement (`dt_s` seconds since the previous one) and returns the new
    /// relative ratio, clamped to `1 ± max_ppm·1e-6`.
    pub fn update(&mut self, _buffered_ms: f64, _target_ms: f64, _dt_s: f64) -> f64 {
        todo!("feat/audio")
    }

    /// The most recently returned ratio.
    pub fn ratio(&self) -> f64 {
        todo!("feat/audio")
    }

    /// Resets the integrator and the ratio to 1.0.
    pub fn reset(&mut self) {
        todo!("feat/audio")
    }
}
