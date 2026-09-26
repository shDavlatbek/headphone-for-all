//! The selftest's capture source: digital silence until it is armed, then a sine tone with a
//! sharp onset. The time of the onset (the capture time of its first sample) is recorded, so
//! the selftest can find the onset in the hub's output and measure the end-to-end latency.
//! Armed from the start it is a plain tone source (the other selftest senders), with an
//! amplitude chosen so that the hub's mix never reaches its limiter.

use std::f64::consts::TAU;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use hfa_audio::AudioFormat;
use hfa_capture::{CaptureError, CaptureSource, PcmSink};

/// Block period of the generator thread (like a sound card delivering 10 ms buffers).
pub const BLOCK_MS: u32 = 10;

/// A generator thread that falls further behind than this (a long stall) skips the missed
/// blocks instead of delivering them in one burst.
const MAX_LAG: Duration = Duration::from_millis(250);

/// Arms the onset and reports when it happened. Cloneable; shared with the source's thread.
#[derive(Debug, Clone, Default)]
pub struct OnsetTrigger {
    /// When [`OnsetTrigger::arm`] was first called.
    armed: Arc<OnceLock<Instant>>,
    onset: Arc<OnceLock<Instant>>,
}

impl OnsetTrigger {
    /// Starts the tone at the next block boundary: the first block whose capture time is not
    /// before this call (so the onset never precedes the arming, even when the generator
    /// thread runs late and catches up).
    pub fn arm(&self) {
        let _ = self.armed.set(Instant::now());
    }

    /// When the first sample of the tone was captured (`None` until the tone started).
    pub fn onset(&self) -> Option<Instant> {
        self.onset.get().copied()
    }
}

/// Silence, then (once [`OnsetTrigger::arm`]ed) a sine tone starting at phase 0, delivered
/// in real time in [`BLOCK_MS`] blocks at 48 kHz stereo. With an armed trigger it is a plain
/// tone source.
pub struct OnsetToneSource {
    freq_hz: f32,
    amplitude: f32,
    trigger: OnsetTrigger,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl OnsetToneSource {
    /// A source for a tone of `freq_hz` and `amplitude` (linear, clamped to 0..=1),
    /// controlled by `trigger`.
    pub fn new(freq_hz: f32, amplitude: f32, trigger: OnsetTrigger) -> Self {
        Self {
            freq_hz,
            amplitude: amplitude.clamp(0.0, 1.0),
            trigger,
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
        }
    }
}

impl CaptureSource for OnsetToneSource {
    fn describe(&self) -> String {
        format!("onset tone {} Hz", self.freq_hz)
    }

    fn format(&self) -> AudioFormat {
        AudioFormat::INTERNAL
    }

    fn start(&mut self, mut sink: PcmSink) -> Result<(), CaptureError> {
        if self.thread.is_some() {
            return Err(CaptureError::AlreadyRunning);
        }
        self.stop.store(false, Ordering::Release);
        let stop = Arc::clone(&self.stop);
        let trigger = self.trigger.clone();
        let amplitude = self.amplitude;
        let format = AudioFormat::INTERNAL;
        let step = TAU * f64::from(self.freq_hz) / f64::from(format.sample_rate);
        let channels = usize::from(format.channels);
        let block_frames = format.frames_for_ms(BLOCK_MS);
        let block_period = Duration::from_millis(u64::from(BLOCK_MS));
        let thread = std::thread::Builder::new()
            .name("hfa-onset-tone".into())
            .spawn(move || {
                let _rt = hfa_capture::rt::promote_current_thread(block_period);
                let mut block = vec![0.0f32; block_frames * channels];
                let mut phase: Option<f64> = None;
                let start = Instant::now();
                let mut blocks: u32 = 0;
                while !stop.load(Ordering::Acquire) {
                    // Block `k` covers [start + k·T, start + (k+1)·T) and is delivered at
                    // its end, like a capture device. A late wake-up delivers every block
                    // that is due, back to back.
                    let block_start = start + block_period * blocks;
                    let due = block_start + block_period;
                    let now = Instant::now();
                    if now < due {
                        std::thread::sleep(due - now);
                        continue;
                    }
                    if now - due > MAX_LAG {
                        // Far behind: resume with the block that ends now.
                        let elapsed = now.saturating_duration_since(start).as_nanos();
                        let due_blocks = elapsed / block_period.as_nanos().max(1);
                        blocks = u32::try_from(due_blocks.saturating_sub(1)).unwrap_or(u32::MAX);
                        continue;
                    }
                    if phase.is_none()
                        && trigger
                            .armed
                            .get()
                            .is_some_and(|armed| block_start >= *armed)
                    {
                        let _ = trigger.onset.set(block_start);
                        phase = Some(0.0);
                    }
                    match phase.as_mut() {
                        None => block.fill(0.0),
                        Some(p) => {
                            for frame in block.chunks_exact_mut(channels) {
                                frame.fill((p.sin() as f32) * amplitude);
                                *p = (*p + step) % TAU;
                            }
                        }
                    }
                    sink.push(&block);
                    blocks = blocks.saturating_add(1);
                }
            })
            .map_err(|e| CaptureError::Backend(format!("cannot start the tone thread: {e}")))?;
        self.thread = Some(thread);
        Ok(())
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for OnsetToneSource {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_until_armed_then_tone() {
        let trigger = OnsetTrigger::default();
        let mut src = OnsetToneSource::new(1000.0, 0.25, trigger.clone());
        let (sink, mut source) = hfa_capture::pcm_ring_with_channels(48_000 * 2, 2);
        src.start(sink).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let mut buf = vec![0.0f32; source.available()];
        source.pull(&mut buf);
        assert!(buf.len() >= 2 * 480 * 5, "blocks delivered: {}", buf.len());
        assert!(buf.iter().all(|v| *v == 0.0), "silent before arming");
        assert!(trigger.onset().is_none());

        let before_arm = Instant::now();
        trigger.arm();
        let after_arm = Instant::now();
        std::thread::sleep(Duration::from_millis(100));
        src.stop();
        let onset = trigger.onset().expect("onset recorded");
        // The next block boundary after arming (a block capture time, so it does not depend
        // on how late the generator thread woke up).
        assert!(
            onset >= before_arm && onset < after_arm + Duration::from_millis(u64::from(BLOCK_MS)),
            "{onset:?} vs {before_arm:?}..{after_arm:?}"
        );
        let mut buf = vec![0.0f32; source.available()];
        source.pull(&mut buf);
        let first = buf.iter().position(|v| v.abs() > 0.01).expect("tone");
        // Starts at phase 0: the first non-zero sample is at the start of a block.
        assert_eq!(first % (2 * 480), 2, "sine starts at a block boundary");
        let peak = buf.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!((peak - 0.25).abs() < 0.01, "peak {peak}");
    }
}
