//! Real-time pacing for the software sources and outputs (tone, WAV file, null): a
//! background thread that handles one block of audio per block period, scheduled against a
//! monotonic clock so that no drift accumulates.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::{CaptureError, Result};

/// If the thread falls further behind than this (e.g. after a system suspend), it skips ahead
/// instead of producing/consuming a burst.
const MAX_LAG: Duration = Duration::from_millis(250);

/// Schedules blocks of `block_frames` frames at `sample_rate` against [`Instant`].
///
/// Block `k` is due at `start + (k + 1) · block_frames / sample_rate`, computed from the total
/// frame count (not by adding up rounded periods), so pacing has no cumulative drift.
#[derive(Debug)]
pub(crate) struct Pacer {
    start: Instant,
    sample_rate: u64,
    block_frames: u64,
    frames_done: u64,
    max_lag_frames: u64,
}

impl Pacer {
    /// Starts the clock now. `sample_rate` and `block_frames` are clamped to at least 1.
    pub(crate) fn new(sample_rate: u32, block_frames: usize) -> Self {
        let sample_rate = u64::from(sample_rate.max(1));
        Self {
            start: Instant::now(),
            sample_rate,
            block_frames: (block_frames as u64).max(1),
            frames_done: 0,
            max_lag_frames: sample_rate * MAX_LAG.as_millis() as u64 / 1000,
        }
    }

    /// Frames per block.
    pub(crate) fn block_frames(&self) -> usize {
        self.block_frames as usize
    }

    fn frames_to_duration(&self, frames: u64) -> Duration {
        let nanos = u128::from(frames) * 1_000_000_000 / u128::from(self.sample_rate);
        Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }

    fn elapsed_frames(&self, now: Instant) -> u64 {
        let nanos = now.saturating_duration_since(self.start).as_nanos();
        u64::try_from(nanos * u128::from(self.sample_rate) / 1_000_000_000).unwrap_or(u64::MAX)
    }

    /// Sleeps until the next block is due and accounts for it. Returns `false` (immediately,
    /// or as soon as it is woken with [`thread::Thread::unpark`]) once `stop` is set.
    pub(crate) fn wait_next(&mut self, stop: &AtomicBool) -> bool {
        loop {
            if stop.load(Ordering::Acquire) {
                return false;
            }
            let due_frames = self.frames_done + self.block_frames;
            let due_at = self.start + self.frames_to_duration(due_frames);
            let now = Instant::now();
            if now >= due_at {
                let elapsed = self.elapsed_frames(now);
                if elapsed > due_frames + self.max_lag_frames {
                    // Far behind: drop the backlog and continue from "now".
                    self.frames_done = elapsed - self.block_frames;
                }
                self.frames_done += self.block_frames;
                return true;
            }
            thread::park_timeout(due_at - now);
        }
    }
}

/// A named background thread with a stop flag. [`PacedThread::stop`] (also run on drop)
/// sets the flag, wakes the thread and joins it.
#[derive(Debug, Default)]
pub(crate) struct PacedThread {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl PacedThread {
    /// Whether a thread is running (started and not yet stopped).
    pub(crate) fn is_running(&self) -> bool {
        self.handle.is_some()
    }

    /// Spawns `body`, which receives the stop flag and must return soon after it is set
    /// (typically by looping on [`Pacer::wait_next`]). The thread is promoted for soft
    /// real-time work with blocks of `period` ([`crate::rt::promote_current_thread`]) before
    /// `body` runs.
    ///
    /// # Errors
    /// [`CaptureError::AlreadyRunning`] if a thread is running, [`CaptureError::Backend`] if
    /// the OS cannot spawn a thread.
    pub(crate) fn spawn<F>(&mut self, name: &str, period: Duration, body: F) -> Result<()>
    where
        F: FnOnce(Arc<AtomicBool>) + Send + 'static,
    {
        if self.handle.is_some() {
            return Err(CaptureError::AlreadyRunning);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name(name.to_owned())
            .spawn(move || {
                let _rt = crate::rt::promote_current_thread(period);
                body(flag);
            })
            .map_err(|e| CaptureError::Backend(format!("cannot spawn thread {name}: {e}")))?;
        self.stop = stop;
        self.handle = Some(handle);
        Ok(())
    }

    /// Stops and joins the thread. Idempotent.
    pub(crate) fn stop(&mut self) {
        if let Some(handle) = self.handle.take() {
            self.stop.store(true, Ordering::Release);
            handle.thread().unpark();
            if handle.join().is_err() {
                tracing::error!("paced audio thread panicked");
            }
        }
    }
}

impl Drop for PacedThread {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Test helper: checks from outside that a running paced thread handles audio at real-time
/// speed, and returns the frames it had handled at the end of the measurement.
///
/// `progress` returns the frames the thread has handled so far (read from ring counters).
/// The thread's start-up latency is not measured: the window starts once the first block has
/// been handled. Over the following `window` of wall-clock time, the frames handled must match
/// the elapsed time within ±`tolerance` (relative; absorbs the scheduling latency of the paced
/// thread on a loaded machine) plus two blocks (at each reading a block may be due but not yet
/// handled, or half accounted). Each reading is bracketed by two clock readings, so a test
/// thread that is descheduled around a reading widens the accepted range instead of failing.
#[cfg(test)]
pub(crate) fn assert_paced_in_real_time(
    progress: impl Fn() -> usize,
    sample_rate: u32,
    block_frames: usize,
    window: Duration,
    tolerance: f64,
) -> usize {
    let read = || {
        let before = Instant::now();
        let frames = progress();
        (before, frames, Instant::now())
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while progress() == 0 {
        assert!(
            Instant::now() < deadline,
            "the paced thread handled nothing in 10 s"
        );
        thread::sleep(Duration::from_millis(1));
    }
    let (a_before, a_frames, a_after) = read();
    thread::sleep(window);
    let (b_before, b_frames, b_after) = read();
    let frames = b_frames.saturating_sub(a_frames) as f64;
    let rate = f64::from(sample_rate);
    let slack = 2.0 * block_frames as f64;
    let shortest = b_before.saturating_duration_since(a_after).as_secs_f64();
    let longest = b_after.saturating_duration_since(a_before).as_secs_f64();
    let (low, high) = (
        shortest * rate * (1.0 - tolerance) - slack,
        longest * rate * (1.0 + tolerance) + slack,
    );
    assert!(
        (low..=high).contains(&frames),
        "{frames} frames handled in {shortest:.4}..{longest:.4} s: expected {low:.0}..={high:.0}"
    );
    b_frames
}

/// Test helper: a paced thread can never get ahead of the clock (block `k` is handled no
/// earlier than its due time), so the `frames` it handled between just before its start and
/// just after its stop (`elapsed`) are at most `elapsed` worth of frames. Scheduling latency
/// only makes it handle less, so this bound holds on any machine.
#[cfg(test)]
pub(crate) fn assert_not_ahead_of_real_time(frames: usize, sample_rate: u32, elapsed: Duration) {
    // One frame of slack for the nanosecond rounding of the due times.
    let most = elapsed.as_secs_f64() * f64::from(sample_rate) + 1.0;
    assert!(
        frames as f64 <= most,
        "{frames} frames handled in {elapsed:?}: faster than real time (at most {most:.0})"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacer_has_no_cumulative_drift() {
        // 441 frames at 44.1 kHz = exactly 10 ms per block.
        let mut pacer = Pacer::new(44_100, 441);
        let stop = AtomicBool::new(false);
        let t0 = Instant::now();
        for _ in 0..30 {
            assert!(pacer.wait_next(&stop));
        }
        let elapsed = t0.elapsed();
        assert!(elapsed >= Duration::from_millis(299), "{elapsed:?}");
        // Upper bound only guards against gross over-sleeping (loaded CI machines).
        assert!(elapsed < Duration::from_millis(600), "{elapsed:?}");
        assert_eq!(pacer.frames_done, 30 * 441);
    }

    #[test]
    fn pacer_catches_up_small_delays_and_skips_large_ones() {
        let mut pacer = Pacer::new(1000, 10); // 10 ms blocks
        let stop = AtomicBool::new(false);
        thread::sleep(Duration::from_millis(55));
        // Behind by ~5 blocks: they are returned back to back.
        let t0 = Instant::now();
        for _ in 0..5 {
            assert!(pacer.wait_next(&stop));
        }
        // Without catch-up these would take 50 ms.
        assert!(
            t0.elapsed() < Duration::from_millis(30),
            "{:?}",
            t0.elapsed()
        );

        thread::sleep(MAX_LAG + Duration::from_millis(100));
        assert!(pacer.wait_next(&stop));
        let behind = pacer.elapsed_frames(Instant::now()) as i64 - pacer.frames_done as i64;
        // Without skipping it would still be ~350 blocks (frames) behind.
        assert!(behind.abs() < 50, "skipped ahead, behind = {behind}");
    }

    #[test]
    fn stop_wakes_a_sleeping_thread() {
        let mut worker = PacedThread::default();
        worker
            .spawn("test-pacer", Duration::from_secs(10), |stop| {
                // One block per 10 s: only the stop flag can end this promptly.
                let mut pacer = Pacer::new(1, 10);
                while pacer.wait_next(&stop) {}
            })
            .expect("spawn");
        assert!(worker.is_running());
        assert_eq!(
            worker.spawn("again", Duration::ZERO, |_| {}).err(),
            Some(CaptureError::AlreadyRunning)
        );
        let t0 = Instant::now();
        worker.stop();
        assert!(t0.elapsed() < Duration::from_secs(1));
        assert!(!worker.is_running());
        worker.stop(); // idempotent
    }
}
