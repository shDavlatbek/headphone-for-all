//! Conversion of native PCM buffers of any rate / channel count to
//! [`AudioFormat::INTERNAL`] (48 kHz stereo), for feeds whose format is only known per buffer
//! (the iOS ReplayKit extension, see `hfa_ext_push_pcm`).

use hfa_audio::convert::to_stereo;
use hfa_audio::resample::StreamResampler;
use hfa_audio::AudioFormat;

use crate::error::{FfiError, Result};

/// Accepted input channel counts.
pub const MIN_CHANNELS: u32 = 1;
/// Accepted input channel counts.
pub const MAX_CHANNELS: u32 = 8;
/// Lowest accepted input sample rate in Hz.
pub const MIN_RATE: u32 = 8_000;
/// Highest accepted input sample rate in Hz.
pub const MAX_RATE: u32 = 192_000;
/// Largest buffer accepted in one call, in samples (all channels): 1 s of 8-channel 192 kHz
/// audio. Protects against absurd sizes from a broken caller.
pub const MAX_SAMPLES_PER_CALL: usize = 192_000 * 8;

/// `true` if `channels` / `rate` are within the accepted ranges.
pub fn format_is_valid(channels: u32, rate: u32) -> bool {
    (MIN_CHANNELS..=MAX_CHANNELS).contains(&channels) && (MIN_RATE..=MAX_RATE).contains(&rate)
}

/// Stateful converter to 48 kHz stereo. Keeps the resampler (and its filter history) while
/// the input format stays the same, and re-creates it when the rate or channel count
/// changes. Buffers are reused, so steady-state conversion does not allocate.
#[derive(Debug, Default)]
pub struct PcmConverter {
    /// Current input format; `None` before the first buffer.
    input: Option<AudioFormat>,
    stereo: Vec<f32>,
    resampled: Vec<f32>,
    resampler: Option<StreamResampler>,
}

impl PcmConverter {
    /// Creates a converter; the format is taken from the first buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// The input format of the last converted buffer.
    pub fn input_format(&self) -> Option<AudioFormat> {
        self.input
    }

    /// Converts one buffer of interleaved samples in `channels` × `rate` Hz to interleaved
    /// 48 kHz stereo and returns it. A trailing partial frame is ignored. With a rate other
    /// than 48 kHz the output lags the input by the resampler's delay, and output appears
    /// once 10 ms of input are buffered.
    ///
    /// # Errors
    /// [`FfiError::InvalidArgument`] for a format outside the accepted ranges;
    /// [`FfiError::Audio`] if the resampler cannot be created.
    pub fn convert(&mut self, samples: &[f32], channels: u32, rate: u32) -> Result<&[f32]> {
        if !format_is_valid(channels, rate) {
            return Err(FfiError::InvalidArgument(format!(
                "pcm format {channels} ch / {rate} Hz (expected {MIN_CHANNELS}..={MAX_CHANNELS} ch, \
                 {MIN_RATE}..={MAX_RATE} Hz)"
            )));
        }
        // Lossless: `channels` <= 8 was checked above.
        let ch = channels as u16;
        let format = AudioFormat::new(rate, ch);
        if self.input != Some(format) {
            self.resampler = if rate == AudioFormat::INTERNAL.sample_rate {
                None
            } else {
                // 10 ms chunks: low latency, and rubato's fixed input chunk stays small.
                let chunk = (rate as usize / 100).max(1);
                Some(StreamResampler::new(
                    AudioFormat::INTERNAL.channels,
                    rate,
                    AudioFormat::INTERNAL.sample_rate,
                    chunk,
                )?)
            };
            self.input = Some(format);
            tracing::debug!(?format, "pcm converter input format");
        }
        to_stereo(samples, ch, &mut self.stereo);
        match self.resampler.as_mut() {
            None => Ok(&self.stereo),
            Some(rs) => {
                self.resampled.clear();
                rs.process(&self.stereo, &mut self.resampled)?;
                Ok(&self.resampled)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_formats() {
        assert!(format_is_valid(1, 8_000));
        assert!(format_is_valid(8, 192_000));
        assert!(!format_is_valid(0, 48_000));
        assert!(!format_is_valid(9, 48_000));
        assert!(!format_is_valid(2, 7_999));
        assert!(!format_is_valid(2, 192_001));
        let mut c = PcmConverter::new();
        assert!(matches!(
            c.convert(&[0.0; 4], 2, 1_000),
            Err(FfiError::InvalidArgument(_))
        ));
        assert_eq!(c.input_format(), None);
    }

    #[test]
    fn stereo_48k_passes_through_unchanged() {
        let mut c = PcmConverter::new();
        let input: Vec<f32> = (0..960).map(|i| (i as f32 / 960.0) - 0.5).collect();
        let out = c.convert(&input, 2, 48_000).expect("convert").to_vec();
        assert_eq!(out, input);
        assert_eq!(c.input_format(), Some(AudioFormat::INTERNAL));
    }

    #[test]
    fn mono_is_duplicated_and_partial_frames_dropped() {
        let mut c = PcmConverter::new();
        let out = c.convert(&[0.1, -0.2, 0.3], 1, 48_000).expect("convert");
        assert_eq!(out, &[0.1, 0.1, -0.2, -0.2, 0.3, 0.3]);
        // 3 samples of stereo = 1 frame + a partial frame.
        let out = c.convert(&[0.5, -0.5, 0.9], 2, 48_000).expect("convert");
        assert_eq!(out, &[0.5, -0.5]);
    }

    #[test]
    fn resamples_44k1_mono_to_48k_stereo() {
        let mut c = PcmConverter::new();
        // 1 s of a 1 kHz tone at 44.1 kHz, pushed in ReplayKit-like 1024-frame buffers.
        let input: Vec<f32> = (0..44_100)
            .map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 44_100.0).sin() * 0.5)
            .collect();
        let mut out = Vec::new();
        for chunk in input.chunks(1024) {
            out.extend_from_slice(c.convert(chunk, 1, 44_100).expect("convert"));
        }
        assert_eq!(out.len() % 2, 0);
        let frames = out.len() / 2;
        // 48 000 frames minus the resampler's start-up delay and one pending chunk.
        assert!((46_500..=48_000).contains(&frames), "{frames} frames");
        for f in out.chunks_exact(2) {
            assert_eq!(f[0], f[1], "mono must reach both channels");
        }
        let peak = out.iter().fold(0.0_f32, |m, x| m.max(x.abs()));
        assert!((0.45..=0.55).contains(&peak), "peak {peak}");
    }

    #[test]
    fn format_change_recreates_the_converter() {
        let mut c = PcmConverter::new();
        c.convert(&[0.0; 882], 1, 44_100).expect("convert");
        assert_eq!(c.input_format(), Some(AudioFormat::new(44_100, 1)));
        let out = c.convert(&[0.25; 12], 6, 48_000).expect("convert").to_vec();
        assert_eq!(c.input_format(), Some(AudioFormat::new(48_000, 6)));
        // 6 channels of 0.25 fold down to 0.25 on both sides (normalized fold-down).
        assert_eq!(out.len(), 4);
        for x in out {
            assert!((x - 0.25).abs() < 1e-6, "{x}");
        }
    }
}
