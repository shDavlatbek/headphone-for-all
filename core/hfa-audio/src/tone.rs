//! Sine test-tone generator.

use crate::format::AudioFormat;

/// Generates a continuous sine wave (same value on every channel), phase-continuous across
/// [`SineGenerator::fill`] calls.
#[derive(Debug, Clone)]
pub struct SineGenerator {
    freq: f32,
    amplitude: f32,
    format: AudioFormat,
}

impl SineGenerator {
    /// Creates a generator for `freq` Hz at linear `amplitude` (0..=1) in `format`.
    pub fn new(freq: f32, amplitude: f32, format: AudioFormat) -> Self {
        Self {
            freq,
            amplitude,
            format,
        }
    }

    /// Frequency in Hz.
    pub fn freq(&self) -> f32 {
        self.freq
    }

    /// Linear amplitude.
    pub fn amplitude(&self) -> f32 {
        self.amplitude
    }

    /// Output format.
    pub fn format(&self) -> AudioFormat {
        self.format
    }

    /// Fills `out` (interleaved; whole frames) with the next samples.
    pub fn fill(&mut self, _out: &mut [f32]) {
        todo!("feat/audio")
    }
}
