//! Sine test-tone generator.

use crate::format::AudioFormat;

/// Generates a continuous sine wave (same value on every channel), phase-continuous across
/// [`SineGenerator::fill`] calls.
#[derive(Debug, Clone)]
pub struct SineGenerator {
    freq: f32,
    amplitude: f32,
    format: AudioFormat,
    /// Phase in cycles, in [0, 1).
    phase: f64,
}

impl SineGenerator {
    /// Creates a generator for `freq` Hz at linear `amplitude` (0..=1) in `format`.
    pub fn new(freq: f32, amplitude: f32, format: AudioFormat) -> Self {
        Self {
            freq,
            amplitude,
            format,
            phase: 0.0,
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
    ///
    /// A trailing partial frame (when `out.len()` is not a multiple of the channel count) is
    /// zeroed and does not advance the phase. A zero channel count or sample rate yields
    /// silence.
    pub fn fill(&mut self, out: &mut [f32]) {
        let channels = usize::from(self.format.channels);
        if channels == 0 || self.format.sample_rate == 0 {
            out.fill(0.0);
            return;
        }
        let step = f64::from(self.freq) / f64::from(self.format.sample_rate);
        let amp = f64::from(self.amplitude);
        let mut chunks = out.chunks_exact_mut(channels);
        for frame in &mut chunks {
            let v = (amp * (std::f64::consts::TAU * self.phase).sin()) as f32;
            frame.fill(v);
            self.phase += step;
            self.phase -= self.phase.floor();
        }
        chunks.into_remainder().fill(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Counts sign changes from negative to non-negative on channel 0.
    fn rising_zero_crossings(buf: &[f32], channels: usize) -> usize {
        let mut n = 0;
        let mut prev = buf[0];
        for frame in buf.chunks_exact(channels).skip(1) {
            if prev < 0.0 && frame[0] >= 0.0 {
                n += 1;
            }
            prev = frame[0];
        }
        n
    }

    #[test]
    fn frequency_and_amplitude() {
        let fmt = AudioFormat::INTERNAL;
        let mut g = SineGenerator::new(440.0, 0.25, fmt);
        // One second, filled in odd-sized blocks to check phase continuity.
        let mut buf = vec![0.0_f32; 48_000 * 2];
        for chunk in buf.chunks_mut(2 * 377) {
            g.fill(chunk);
        }
        let crossings = rising_zero_crossings(&buf, 2);
        assert!((439..=441).contains(&crossings), "crossings {crossings}");
        let peak = buf.iter().fold(0.0_f32, |m, x| m.max(x.abs()));
        assert!((peak - 0.25).abs() < 1e-3, "peak {peak}");
        // Both channels carry the same value.
        assert!(buf.chunks_exact(2).all(|f| f[0] == f[1]));
        // No discontinuity: consecutive samples differ by at most amp * 2*pi*f/fs.
        let max_step = 0.25 * std::f32::consts::TAU * 440.0 / 48_000.0 + 1e-5;
        assert!(buf
            .chunks_exact(2)
            .zip(buf.chunks_exact(2).skip(1))
            .all(|(a, b)| (a[0] - b[0]).abs() <= max_step));
    }

    #[test]
    fn partial_frame_is_zeroed() {
        let mut g = SineGenerator::new(1000.0, 1.0, AudioFormat::new(48_000, 2));
        let mut buf = [9.0_f32; 5];
        g.fill(&mut buf);
        assert_eq!(buf[4], 0.0);
        assert_eq!(buf[0], 0.0); // sin(0)
        assert!(buf[2] > 0.0);
        assert_eq!(g.freq(), 1000.0);
        assert_eq!(g.amplitude(), 1.0);
        assert_eq!(g.format(), AudioFormat::new(48_000, 2));
    }
}
