//! Variable-ratio streaming resampler built on `rubato`.

use crate::Result;

/// Streaming resampler for interleaved `f32`, with a nominal `in_rate → out_rate` conversion
/// plus a small relative ratio adjustment for drift correction.
pub struct StreamResampler {
    channels: u16,
    in_rate: u32,
    out_rate: u32,
    chunk_frames: usize,
}

impl StreamResampler {
    /// Creates a resampler. `chunk_frames` is the internal processing block size in input
    /// frames (e.g. one 10 ms frame = 480).
    ///
    /// # Errors
    /// [`crate::AudioError::InvalidConfig`] or [`crate::AudioError::Resample`].
    pub fn new(channels: u16, in_rate: u32, out_rate: u32, chunk_frames: usize) -> Result<Self> {
        let _ = (channels, in_rate, out_rate, chunk_frames);
        todo!("feat/audio")
    }

    /// Channel count.
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Nominal input rate.
    pub fn in_rate(&self) -> u32 {
        self.in_rate
    }

    /// Nominal output rate.
    pub fn out_rate(&self) -> u32 {
        self.out_rate
    }

    /// Internal block size in input frames.
    pub fn chunk_frames(&self) -> usize {
        self.chunk_frames
    }

    /// Sets the relative ratio multiplier on top of `out_rate / in_rate` (1.0 = nominal),
    /// typically from [`crate::DriftController::update`]. Changes are applied smoothly.
    ///
    /// # Errors
    /// [`crate::AudioError::Resample`] if the ratio is outside the supported range.
    pub fn set_ratio_relative(&mut self, _ratio: f64) -> Result<()> {
        todo!("feat/audio")
    }

    /// Feeds interleaved `input` (any length; partial chunks are buffered internally) and
    /// **appends** all produced interleaved output to `out`.
    ///
    /// # Errors
    /// [`crate::AudioError::Resample`].
    pub fn process(&mut self, _input: &[f32], _out: &mut Vec<f32>) -> Result<()> {
        todo!("feat/audio")
    }

    /// Clears internal buffers and filter state.
    pub fn reset(&mut self) {
        todo!("feat/audio")
    }
}
