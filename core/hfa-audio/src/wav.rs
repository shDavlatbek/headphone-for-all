//! Thin helpers over `hound` for 32-bit float WAV files.

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use crate::format::AudioFormat;
use crate::Result;

/// Writes interleaved `f32` samples to a 32-bit float WAV file.
pub struct WavWriter {
    inner: hound::WavWriter<BufWriter<File>>,
    format: AudioFormat,
}

impl WavWriter {
    /// Creates (truncates) `path`.
    ///
    /// # Errors
    /// [`crate::AudioError::Wav`].
    pub fn create(_path: &Path, _format: AudioFormat) -> Result<Self> {
        todo!("feat/audio")
    }

    /// The file's format.
    pub fn format(&self) -> AudioFormat {
        self.format
    }

    /// Appends interleaved samples.
    ///
    /// # Errors
    /// [`crate::AudioError::Wav`].
    pub fn write(&mut self, _interleaved: &[f32]) -> Result<()> {
        let _ = &self.inner;
        todo!("feat/audio")
    }

    /// Flushes and finalizes the header.
    ///
    /// # Errors
    /// [`crate::AudioError::Wav`].
    pub fn finalize(self) -> Result<()> {
        todo!("feat/audio")
    }
}

/// Reads a whole WAV file (16/24/32-bit int or 32-bit float) as interleaved `f32` in [-1, 1].
///
/// # Errors
/// [`crate::AudioError::Wav`].
pub fn read_wav(_path: &Path) -> Result<(AudioFormat, Vec<f32>)> {
    todo!("feat/audio")
}
