//! Test-tone capture source: a real-time-paced thread generating a sine wave.

use std::f64::consts::TAU;

use hfa_audio::AudioFormat;

use crate::pacer::{PacedThread, Pacer};
use crate::ring::PcmSink;
use crate::{CaptureSource, Result};

/// Amplitude of the generated tone (about −12 dBFS).
pub const TONE_AMPLITUDE: f32 = 0.25;

/// Block period of the generator thread.
pub(crate) const BLOCK_MS: u32 = 10;

/// Generates a sine tone in real time: one 10 ms block per 10 ms on a background thread,
/// scheduled against a monotonic clock (no cumulative drift). Every channel carries the same
/// signal.
pub struct ToneSource {
    freq_hz: f32,
    format: AudioFormat,
    worker: PacedThread,
}

impl ToneSource {
    /// Creates a tone source (amplitude [`TONE_AMPLITUDE`], i.e. about −12 dBFS).
    pub fn new(freq_hz: f32, format: AudioFormat) -> Self {
        Self {
            freq_hz,
            format,
            worker: PacedThread::default(),
        }
    }

    /// Tone frequency in Hz.
    pub fn freq_hz(&self) -> f32 {
        self.freq_hz
    }
}

/// Phase-continuous sine oscillator writing interleaved frames.
struct Oscillator {
    phase: f64,
    step: f64,
    channels: usize,
}

impl Oscillator {
    fn new(freq_hz: f32, format: AudioFormat) -> Self {
        Self {
            phase: 0.0,
            step: TAU * f64::from(freq_hz) / f64::from(format.sample_rate.max(1)),
            channels: usize::from(format.channels.max(1)),
        }
    }

    fn fill(&mut self, out: &mut [f32]) {
        for frame in out.chunks_exact_mut(self.channels) {
            let v = (self.phase.sin() as f32) * TONE_AMPLITUDE;
            frame.fill(v);
            self.phase = (self.phase + self.step) % TAU;
        }
    }
}

impl CaptureSource for ToneSource {
    fn describe(&self) -> String {
        format!("Test tone {} Hz", self.freq_hz)
    }

    fn format(&self) -> AudioFormat {
        self.format
    }

    fn start(&mut self, mut sink: PcmSink) -> Result<()> {
        let format = self.format;
        let mut osc = Oscillator::new(self.freq_hz, format);
        self.worker.spawn("hfa-tone", move |stop| {
            let mut pacer = Pacer::new(format.sample_rate, format.frames_for_ms(BLOCK_MS));
            let mut block = vec![0.0f32; pacer.block_frames() * usize::from(format.channels)];
            while pacer.wait_next(&stop) {
                osc.fill(&mut block);
                sink.push(&block);
            }
        })
    }

    fn stop(&mut self) {
        self.worker.stop();
    }
}

impl Drop for ToneSource {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::pacer::assert_real_time;
    use crate::ring::pcm_ring_with_channels;
    use crate::CaptureError;

    #[test]
    fn tone_is_paced_in_real_time() {
        let format = AudioFormat::INTERNAL;
        let mut tone = ToneSource::new(440.0, format);
        assert_eq!(tone.format(), format);
        let (sink, mut source) = pcm_ring_with_channels(format.samples_for_ms(3000), 2);
        let overruns = sink.stats();
        let t0 = Instant::now();
        tone.start(sink).expect("start");
        std::thread::sleep(Duration::from_secs(1));
        tone.stop();
        let elapsed = t0.elapsed();
        let frames = source.available() / 2;
        // ~1 s must give 48000 ± 5 % frames; measured against the real elapsed time so an
        // oversleeping test thread on a loaded machine does not make the test flaky.
        assert_real_time(frames, 48_000, elapsed, 480, 0.05);
        assert_eq!(frames % 480, 0, "whole 10 ms blocks");
        assert_eq!(overruns.count(), 0);

        // The signal is a stereo sine at the right frequency and amplitude.
        let mut buf = vec![0.0; frames * 2];
        assert_eq!(source.pull(&mut buf), frames * 2);
        let peak = buf.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((peak - TONE_AMPLITUDE).abs() < 1e-3, "peak {peak}");
        assert!(buf.chunks_exact(2).all(|f| f[0] == f[1]));
        let rising_zero_crossings = buf
            .chunks_exact(2)
            .map(|f| f[0])
            .collect::<Vec<_>>()
            .windows(2)
            .filter(|w| w[0] < 0.0 && w[1] >= 0.0)
            .count();
        let expected = 440.0 * frames as f64 / 48_000.0;
        assert!(
            (rising_zero_crossings as f64 - expected).abs() <= 2.0,
            "{rising_zero_crossings} cycles, expected {expected}"
        );
    }

    #[test]
    fn start_twice_fails_and_stop_is_idempotent() {
        let mut tone = ToneSource::new(1000.0, AudioFormat::new(8000, 1));
        let (sink, mut source) = pcm_ring_with_channels(8000, 1);
        let (sink2, _unused) = pcm_ring_with_channels(8000, 1);
        tone.start(sink).expect("start");
        assert_eq!(tone.start(sink2).err(), Some(CaptureError::AlreadyRunning));
        std::thread::sleep(Duration::from_millis(50));
        tone.stop();
        tone.stop();
        let produced = source.available();
        assert!(produced > 0);
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(source.available(), produced, "nothing after stop");
        let mut buf = vec![0.0; produced];
        source.pull(&mut buf);

        // Restartable.
        let (sink3, source3) = pcm_ring_with_channels(8000, 1);
        tone.start(sink3).expect("restart");
        std::thread::sleep(Duration::from_millis(50));
        drop(tone); // Drop stops the thread.
        assert!(source3.available() > 0);
    }
}
