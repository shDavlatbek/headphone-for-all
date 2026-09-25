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
pub fn amplitude_to_db(_amplitude: f32) -> f32 {
    todo!("feat/audio")
}

/// Converts dB to a linear gain factor.
pub fn db_to_amplitude(_db: f32) -> f32 {
    todo!("feat/audio")
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
    pub fn process(&mut self, _interleaved: &[f32]) -> Level {
        todo!("feat/audio")
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
