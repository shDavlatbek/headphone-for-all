//! Variable-ratio streaming resampler built on `rubato`.
//!
//! # Ratio convention
//!
//! The resample ratio is **output frames per input frame** (rubato's convention):
//! `ratio = (out_rate / in_rate) · relative`. A relative ratio **> 1** produces slightly *more*
//! output per input (the input is consumed more slowly, relative to a fixed output clock); a
//! relative ratio **< 1** produces *less* output, so a consumer pulling a fixed amount of
//! output per tick consumes its input slightly **faster**. This is the convention of
//! [`crate::DriftController::update`]: when the hub's jitter buffer is fuller than its target,
//! the controller returns a ratio < 1 and the buffer drains.
//!
//! The resampler is `rubato::Async` with a 128-tap sinc interpolator (Blackman-Harris²
//! window, automatic cutoff, cubic interpolation of an oversampled filter), fixed *input*
//! chunk size, and ramps ratio changes linearly over one chunk.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, FixedAsync, Resampler, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};

use crate::error::AudioError;
use crate::Result;

/// Largest supported relative ratio deviation: the relative ratio must stay within
/// `1 / MAX_RELATIVE_RATIO ..= MAX_RELATIVE_RATIO` (±5 %, far beyond any clock drift).
pub const MAX_RELATIVE_RATIO: f64 = 1.05;

/// Length of the sinc interpolation filter in taps.
const SINC_LEN: usize = 128;

fn resample_err(e: impl std::fmt::Display) -> AudioError {
    AudioError::Resample(e.to_string())
}

/// Streaming resampler for interleaved `f32`, with a nominal `in_rate → out_rate` conversion
/// plus a small relative ratio adjustment for drift correction.
pub struct StreamResampler {
    channels: u16,
    in_rate: u32,
    out_rate: u32,
    chunk_frames: usize,
    relative: f64,
    inner: Async<f32>,
    /// Interleaved input waiting for a full chunk (capacity reused).
    pending: Vec<f32>,
    /// Interleaved output scratch of `output_frames_max()` frames.
    scratch: Vec<f32>,
}

impl std::fmt::Debug for StreamResampler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamResampler")
            .field("channels", &self.channels)
            .field("in_rate", &self.in_rate)
            .field("out_rate", &self.out_rate)
            .field("chunk_frames", &self.chunk_frames)
            .field("relative", &self.relative)
            .field("pending_frames", &self.pending_frames())
            .finish_non_exhaustive()
    }
}

impl StreamResampler {
    /// Creates a resampler. `chunk_frames` is the internal processing block size in input
    /// frames (e.g. one 10 ms frame = 480).
    ///
    /// # Errors
    /// [`crate::AudioError::InvalidConfig`] or [`crate::AudioError::Resample`].
    pub fn new(channels: u16, in_rate: u32, out_rate: u32, chunk_frames: usize) -> Result<Self> {
        if channels == 0 || in_rate == 0 || out_rate == 0 || chunk_frames == 0 {
            return Err(AudioError::InvalidConfig(format!(
                "resampler channels {channels}, rates {in_rate} -> {out_rate}, chunk {chunk_frames}"
            )));
        }
        let params = SincInterpolationParameters::new(SINC_LEN, WindowFunction::BlackmanHarris2)
            .oversampling_factor(128)
            .interpolation(SincInterpolationType::Cubic);
        let ratio = f64::from(out_rate) / f64::from(in_rate);
        let inner = Async::<f32>::new_sinc(
            ratio,
            MAX_RELATIVE_RATIO,
            &params,
            chunk_frames,
            usize::from(channels),
            FixedAsync::Input,
        )
        .map_err(resample_err)?;
        let ch = usize::from(channels);
        let scratch = vec![0.0; inner.output_frames_max() * ch];
        Ok(Self {
            channels,
            in_rate,
            out_rate,
            chunk_frames,
            relative: 1.0,
            inner,
            pending: Vec::with_capacity(chunk_frames * ch * 2),
            scratch,
        })
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

    /// The current relative ratio (1.0 = nominal).
    pub fn ratio_relative(&self) -> f64 {
        self.relative
    }

    /// Input frames buffered internally, waiting for a full chunk.
    pub fn pending_frames(&self) -> usize {
        self.pending.len() / usize::from(self.channels)
    }

    /// Delay of the resampling filter in output frames (the output lags the input by this).
    pub fn output_delay(&self) -> usize {
        self.inner.output_delay()
    }

    /// Sets the relative ratio multiplier on top of `out_rate / in_rate` (1.0 = nominal),
    /// typically from [`crate::DriftController::update`]. Changes are applied smoothly (ramped
    /// linearly over the next chunk). See the module docs for the direction convention.
    ///
    /// # Errors
    /// [`crate::AudioError::Resample`] if the ratio is not finite or outside
    /// `1 / MAX_RELATIVE_RATIO ..= MAX_RELATIVE_RATIO`.
    pub fn set_ratio_relative(&mut self, ratio: f64) -> Result<()> {
        if !ratio.is_finite() || !(1.0 / MAX_RELATIVE_RATIO..=MAX_RELATIVE_RATIO).contains(&ratio) {
            return Err(AudioError::Resample(format!(
                "relative ratio {ratio} outside ±{:.0} %",
                (MAX_RELATIVE_RATIO - 1.0) * 100.0
            )));
        }
        rubato::Adjustable::set_resample_ratio_relative(&mut self.inner, ratio, true)
            .map_err(resample_err)?;
        self.relative = ratio;
        Ok(())
    }

    /// Feeds interleaved `input` (any length; partial chunks are buffered internally) and
    /// **appends** all produced interleaved output to `out`.
    ///
    /// A trailing partial frame is buffered too (it completes with the next call). After
    /// warm-up this allocates only when `out` has to grow.
    ///
    /// # Errors
    /// [`crate::AudioError::Resample`].
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<()> {
        let ch = usize::from(self.channels);
        self.pending.extend_from_slice(input);
        let mut consumed = 0;
        loop {
            let need = self.inner.input_frames_next();
            let avail = (self.pending.len() - consumed) / ch;
            if avail < need {
                break;
            }
            let in_slice = &self.pending[consumed..consumed + need * ch];
            let adapter_in = InterleavedSlice::new(in_slice, ch, need).map_err(resample_err)?;
            let out_frames = self.scratch.len() / ch;
            let mut adapter_out = InterleavedSlice::new_mut(&mut self.scratch[..], ch, out_frames)
                .map_err(resample_err)?;
            let (read, written) = self
                .inner
                .process_into_buffer(&adapter_in, &mut adapter_out, None)
                .map_err(resample_err)?;
            out.extend_from_slice(&self.scratch[..written * ch]);
            consumed += read * ch;
            if read == 0 {
                break;
            }
        }
        if consumed > 0 {
            self.pending.copy_within(consumed.., 0);
            let left = self.pending.len() - consumed;
            self.pending.truncate(left);
        }
        Ok(())
    }

    /// Clears internal buffers and filter state. The relative ratio is kept.
    pub fn reset(&mut self) {
        self.inner.reset();
        self.pending.clear();
        if self.relative != 1.0 {
            // Cannot fail: `relative` was validated when it was set.
            let _ = rubato::Adjustable::set_resample_ratio_relative(
                &mut self.inner,
                self.relative,
                false,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::AudioFormat;
    use crate::tone::SineGenerator;

    /// Frequency of channel 0 from interpolated rising zero crossings.
    fn freq(x: &[f32], channels: usize, rate: f64) -> f64 {
        let mono: Vec<f32> = x.chunks_exact(channels).map(|f| f[0]).collect();
        let mut c = Vec::new();
        for i in 1..mono.len() {
            if mono[i - 1] < 0.0 && mono[i] >= 0.0 {
                c.push((i - 1) as f64 + f64::from(mono[i - 1]) / f64::from(mono[i - 1] - mono[i]));
            }
        }
        (c.len() - 1) as f64 * rate / (c[c.len() - 1] - c[0])
    }

    #[test]
    fn converts_44k1_to_48k_keeping_frequency_and_length() {
        let mut rs = StreamResampler::new(2, 44_100, 48_000, 441).unwrap();
        let mut gen = SineGenerator::new(1000.0, 0.5, AudioFormat::new(44_100, 2));
        let mut input = vec![0.0; 441_000 * 2];
        gen.fill(&mut input);
        let mut out = Vec::new();
        // 10 s fed in irregular chunk sizes, including odd sample counts (partial frames).
        let sizes: [usize; 6] = [441 * 2, 100, 7, 1_001, 2_048, 3];
        let (mut pos, mut i) = (0, 0);
        while pos < input.len() {
            let n = sizes[i % sizes.len()].min(input.len() - pos);
            rs.process(&input[pos..pos + n], &mut out).unwrap();
            pos += n;
            i += 1;
        }
        let total_in = input.len();
        assert_eq!(out.len() % 2, 0);
        let in_frames = total_in / 2;
        let expected = in_frames as f64 * 48_000.0 / 44_100.0;
        let got = (out.len() / 2) as f64;
        // Output lags by the filter delay plus at most one pending chunk.
        let slack = rs.output_delay() as f64 + 441.0 * 48.0 / 44.1 + 2.0;
        assert!(
            got <= expected + 2.0 && got >= expected - slack,
            "{got} vs {expected}"
        );
        let f = freq(&out[rs.output_delay() * 2 + 4_800..], 2, 48_000.0);
        assert!((f - 1000.0).abs() < 0.2, "frequency {f}");
        let peak = out[48_000..].iter().fold(0.0_f32, |m, v| m.max(v.abs()));
        assert!((peak - 0.5).abs() < 0.01, "peak {peak}");
    }

    #[test]
    fn relative_ratio_changes_output_count() {
        let chunk = 480;
        let mut base = StreamResampler::new(1, 48_000, 48_000, chunk).unwrap();
        let mut fast = StreamResampler::new(1, 48_000, 48_000, chunk).unwrap();
        fast.set_ratio_relative(1.001).unwrap();
        assert_eq!(fast.ratio_relative(), 1.001);
        let mut gen = SineGenerator::new(440.0, 0.5, AudioFormat::new(48_000, 1));
        let mut buf = vec![0.0; chunk];
        let (mut a, mut b) = (Vec::new(), Vec::new());
        for _ in 0..1_000 {
            gen.fill(&mut buf);
            base.process(&buf, &mut a).unwrap();
            fast.process(&buf, &mut b).unwrap();
        }
        let n_in = 480_000.0;
        let ra = a.len() as f64 / n_in;
        let rb = b.len() as f64 / n_in;
        assert!((ra - 1.0).abs() < 1e-4, "nominal {ra}");
        let extra = b.len() as f64 / a.len() as f64 - 1.0;
        assert!((extra - 0.001).abs() < 5e-5, "extra {extra} ({rb})");
        // The pitch drops by the same factor.
        let f = freq(&b[48_000..], 1, 48_000.0);
        assert!((f - 440.0 / 1.001).abs() < 0.05, "frequency {f}");
    }

    #[test]
    fn rejects_bad_parameters() {
        assert!(matches!(
            StreamResampler::new(0, 48_000, 48_000, 480),
            Err(AudioError::InvalidConfig(_))
        ));
        assert!(matches!(
            StreamResampler::new(2, 48_000, 48_000, 0),
            Err(AudioError::InvalidConfig(_))
        ));
        let mut rs = StreamResampler::new(2, 48_000, 48_000, 480).unwrap();
        for bad in [0.5, 1.2, f64::NAN, f64::INFINITY] {
            assert!(matches!(
                rs.set_ratio_relative(bad),
                Err(AudioError::Resample(_))
            ));
        }
        rs.set_ratio_relative(0.998).unwrap();
        assert_eq!(rs.ratio_relative(), 0.998);
    }

    #[test]
    fn buffers_partial_chunks_and_resets() {
        let mut rs = StreamResampler::new(2, 48_000, 48_000, 480).unwrap();
        let mut out = Vec::new();
        rs.process(&[0.1; 479 * 2 + 1], &mut out).unwrap();
        assert!(out.is_empty());
        assert_eq!(rs.pending_frames(), 479);
        rs.process(&[0.1; 1], &mut out).unwrap();
        assert!(!out.is_empty() && out.len() % 2 == 0);
        assert_eq!(rs.pending_frames(), 0);
        rs.set_ratio_relative(1.002).unwrap();
        rs.process(&[0.1; 100], &mut out).unwrap();
        rs.reset();
        assert_eq!(rs.pending_frames(), 0);
        assert_eq!(rs.ratio_relative(), 1.002);
        out.clear();
        rs.process(&[0.0; 480 * 2], &mut out).unwrap();
        assert!(out.iter().all(|x| *x == 0.0), "state cleared");
    }

    #[test]
    fn no_allocation_growth_after_warmup() {
        let mut rs = StreamResampler::new(2, 44_100, 48_000, 441).unwrap();
        let mut out = Vec::with_capacity(8_192);
        let input = vec![0.25; 441 * 2];
        rs.process(&input, &mut out).unwrap();
        let (pending_cap, scratch_cap) = (rs.pending.capacity(), rs.scratch.capacity());
        for _ in 0..500 {
            out.clear();
            rs.process(&input, &mut out).unwrap();
            rs.process(&input[..300], &mut out).unwrap();
            rs.process(&input[300..], &mut out).unwrap();
        }
        assert_eq!(rs.pending.capacity(), pending_cap);
        assert_eq!(rs.scratch.capacity(), scratch_cap);
    }
}
