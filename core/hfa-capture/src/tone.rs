//! Test-tone capture source: a real-time-paced thread generating a sine wave.

use hfa_audio::AudioFormat;

use crate::ring::PcmSink;
use crate::{CaptureSource, Result};

/// Generates a sine tone in real time (one block every few ms on a background thread).
pub struct ToneSource {
    freq_hz: f32,
    format: AudioFormat,
}

impl ToneSource {
    /// Creates a tone source (amplitude 0.25, i.e. about −12 dBFS).
    pub fn new(freq_hz: f32, format: AudioFormat) -> Self {
        Self { freq_hz, format }
    }

    /// Tone frequency in Hz.
    pub fn freq_hz(&self) -> f32 {
        self.freq_hz
    }
}

impl CaptureSource for ToneSource {
    fn describe(&self) -> String {
        format!("Test tone {} Hz", self.freq_hz)
    }

    fn format(&self) -> AudioFormat {
        self.format
    }

    fn start(&mut self, _sink: PcmSink) -> Result<()> {
        todo!("feat/capture")
    }

    fn stop(&mut self) {
        todo!("feat/capture")
    }
}
