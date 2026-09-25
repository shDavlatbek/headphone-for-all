//! Device playback through `cpal` (WASAPI, Core Audio, ALSA/PipeWire, AAudio).
//!
//! The cpal callback only copies from the [`PcmSource`] ring (converting channel count if the
//! device is not stereo). It never allocates, locks, logs or blocks.

use hfa_audio::AudioFormat;

use crate::ring::PcmSource;
use crate::{AudioOutput, Result};

/// Plays audio on the default or a named output device.
pub struct CpalOutput {
    device_name: String,
    format: AudioFormat,
    buffer_ms: u32,
}

impl CpalOutput {
    /// Opens the named device (`None` = default output). The format is the device's default
    /// output config (sample rate / channels).
    ///
    /// # Errors
    /// [`crate::CaptureError::NotFound`] or [`crate::CaptureError::Backend`].
    pub fn open(_device: Option<&str>, _buffer_ms: u32) -> Result<Self> {
        todo!("feat/capture")
    }

    /// Name of the opened device.
    pub fn device_name(&self) -> &str {
        &self.device_name
    }

    /// Requested buffer size in ms.
    pub fn buffer_ms(&self) -> u32 {
        self.buffer_ms
    }
}

impl AudioOutput for CpalOutput {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn start(&mut self, _source: PcmSource) -> Result<()> {
        todo!("feat/capture")
    }

    fn stop(&mut self) {
        todo!("feat/capture")
    }

    fn latency_ms(&self) -> Option<f32> {
        todo!("feat/capture")
    }
}

/// Names of all output devices of the default cpal host.
///
/// # Errors
/// [`crate::CaptureError::Backend`].
pub fn list_devices() -> Result<Vec<String>> {
    todo!("feat/capture")
}
