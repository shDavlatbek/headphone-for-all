//! Non-device outputs, paced in real time by a background thread: WAV file and null.
//! Used by `hfa hub --out wav:<path>|null` and the selftest.

use std::path::{Path, PathBuf};

use hfa_audio::AudioFormat;

use crate::ring::PcmSource;
use crate::{AudioOutput, Result};

/// Writes the pulled audio to a 32-bit float WAV file in real time.
pub struct WavFileOutput {
    path: PathBuf,
    format: AudioFormat,
    buffer_ms: u32,
}

impl WavFileOutput {
    /// Creates the output (the file is created on `start`).
    ///
    /// # Errors
    /// [`crate::CaptureError::Io`] if the parent directory does not exist.
    pub fn create(_path: &Path, _format: AudioFormat, _buffer_ms: u32) -> Result<Self> {
        todo!("feat/capture")
    }

    /// The file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Pull block size in ms.
    pub fn buffer_ms(&self) -> u32 {
        self.buffer_ms
    }
}

impl AudioOutput for WavFileOutput {
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

/// Discards the pulled audio in real time.
pub struct NullOutput {
    format: AudioFormat,
    buffer_ms: u32,
}

impl NullOutput {
    /// Creates a null output.
    pub fn new(format: AudioFormat, buffer_ms: u32) -> Self {
        Self { format, buffer_ms }
    }

    /// Pull block size in ms.
    pub fn buffer_ms(&self) -> u32 {
        self.buffer_ms
    }
}

impl AudioOutput for NullOutput {
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
