//! WAV-file capture source: plays a file in real time on a background thread, looping.

use std::path::{Path, PathBuf};

use hfa_audio::AudioFormat;

use crate::ring::PcmSink;
use crate::{CaptureSource, Result};

/// Streams a WAV file into the sink at real-time pace, looping at the end.
pub struct WavFileSource {
    path: PathBuf,
    format: AudioFormat,
}

impl WavFileSource {
    /// Opens and fully reads `path` (so `format` is known before `start`).
    ///
    /// # Errors
    /// [`crate::CaptureError::Audio`] / [`crate::CaptureError::Io`] if the file cannot be read.
    pub fn open(_path: &Path) -> Result<Self> {
        todo!("feat/capture")
    }

    /// The file path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl CaptureSource for WavFileSource {
    fn describe(&self) -> String {
        format!("WAV file {}", self.path.display())
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
