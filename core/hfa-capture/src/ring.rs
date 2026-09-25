//! Lock-free SPSC PCM ring (`rtrb`) connecting audio callbacks with the rest of the program.
//! Both ends are real-time safe: no allocation, no locks, no syscalls.
//!
//! The ring knows the channel count of the interleaved stream it carries
//! ([`pcm_ring_with_channels`]), so it never splits a frame: [`PcmSink::push`] writes whole
//! frames only and [`PcmSource::pull`] reads whole frames only. A ring made with
//! [`pcm_ring`] has one "channel", i.e. works sample by sample.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// A cloneable, read-only view of one ring counter (overruns of a [`PcmSink`] or underruns
/// of a [`PcmSource`]).
///
/// Take it with [`PcmSink::stats`] / [`PcmSource::stats`] **before** moving the ring end into
/// [`crate::CaptureSource::start`] / [`crate::AudioOutput::start`]; it keeps counting
/// afterwards, so the sender status, the hub and `hfa selftest` can report glitches.
#[derive(Debug, Clone)]
pub struct RingStats {
    counter: Arc<AtomicU64>,
}

impl RingStats {
    /// Current value of the counter, in samples.
    pub fn count(&self) -> u64 {
        self.counter.load(Ordering::Relaxed)
    }
}

/// Creates a sample-granular ring (channel count 1) holding up to `capacity_samples`
/// interleaved `f32` samples (at least 1).
///
/// Prefer [`pcm_ring_with_channels`] for multi-channel audio: it never splits a frame.
pub fn pcm_ring(capacity_samples: usize) -> (PcmSink, PcmSource) {
    pcm_ring_with_channels(capacity_samples, 1)
}

/// Creates a ring for interleaved audio with `channels` channels per frame.
///
/// The capacity is `capacity_samples` rounded down to whole frames, but at least one frame.
/// `channels == 0` is treated as 1. Both ends only ever move whole frames.
pub fn pcm_ring_with_channels(capacity_samples: usize, channels: u16) -> (PcmSink, PcmSource) {
    let channels = usize::from(channels.max(1));
    let capacity = (capacity_samples / channels * channels).max(channels);
    let (producer, consumer) = rtrb::RingBuffer::<f32>::new(capacity);
    (
        PcmSink {
            producer,
            channels,
            overruns: Arc::new(AtomicU64::new(0)),
        },
        PcmSource {
            consumer,
            channels,
            underruns: Arc::new(AtomicU64::new(0)),
        },
    )
}

/// Producer end (written by capture callbacks / the hub mixer thread).
pub struct PcmSink {
    producer: rtrb::Producer<f32>,
    channels: usize,
    overruns: Arc<AtomicU64>,
}

impl PcmSink {
    /// Writes as many **whole frames** as fit and returns how many samples were written
    /// (always a multiple of [`PcmSink::channels`]). Every sample that was not written
    /// (ring full, or a trailing partial frame) is dropped and counted in
    /// [`PcmSink::overruns`].
    ///
    /// Real-time safe: no allocation, no lock, no syscall.
    pub fn push(&mut self, interleaved: &[f32]) -> usize {
        let fits = interleaved.len().min(self.producer.slots());
        let n = fits / self.channels * self.channels;
        if n > 0 {
            match self.producer.write_chunk_uninit(n) {
                Ok(chunk) => {
                    // `fill_from_iter` commits exactly the items it wrote (all `n` here).
                    chunk.fill_from_iter(interleaved[..n].iter().copied());
                }
                // Unreachable: `slots()` said `n` fit and only this producer can reduce it.
                Err(_) => return self.count_dropped(interleaved.len()),
            }
        }
        self.count_dropped(interleaved.len() - n);
        n
    }

    fn count_dropped(&self, dropped: usize) -> usize {
        if dropped > 0 {
            self.overruns.fetch_add(dropped as u64, Ordering::Relaxed);
        }
        0
    }

    /// Total number of samples dropped because the ring was full.
    pub fn overruns(&self) -> u64 {
        self.overruns.load(Ordering::Relaxed)
    }

    /// A shared handle to the overrun counter that stays valid after this sink is moved.
    pub fn stats(&self) -> RingStats {
        RingStats {
            counter: Arc::clone(&self.overruns),
        }
    }

    /// Free space in samples.
    pub fn free(&self) -> usize {
        self.producer.slots()
    }

    /// Ring capacity in samples (a multiple of [`PcmSink::channels`]).
    pub fn capacity(&self) -> usize {
        self.producer.buffer().capacity()
    }

    /// Interleaved channels per frame this ring was created for.
    pub fn channels(&self) -> u16 {
        // Created from a `u16` in `pcm_ring_with_channels`.
        u16::try_from(self.channels).unwrap_or(u16::MAX)
    }
}

/// Consumer end (read by output callbacks / the sender encoder thread).
pub struct PcmSource {
    consumer: rtrb::Consumer<f32>,
    channels: usize,
    underruns: Arc<AtomicU64>,
}

impl PcmSource {
    /// Fills `out` with as many **whole frames** as are available and returns how many
    /// samples were real (a multiple of [`PcmSource::channels`]); the rest of `out` is
    /// zero-filled and counted in [`PcmSource::underruns`].
    ///
    /// `out.len()` should be a multiple of the channel count; a trailing partial frame is
    /// always zero-filled. Real-time safe: no allocation, no lock, no syscall.
    pub fn pull(&mut self, out: &mut [f32]) -> usize {
        let avail = out.len().min(self.consumer.slots());
        let mut n = avail / self.channels * self.channels;
        if n > 0 {
            match self.consumer.read_chunk(n) {
                Ok(chunk) => {
                    let (first, second) = chunk.as_slices();
                    out[..first.len()].copy_from_slice(first);
                    out[first.len()..n].copy_from_slice(second);
                    chunk.commit_all();
                }
                // Unreachable: `slots()` said `n` were readable and only this consumer can
                // reduce it. Treat as an underrun rather than panicking.
                Err(_) => n = 0,
            }
        }
        out[n..].fill(0.0);
        let missing = out.len() - n;
        if missing > 0 {
            self.underruns.fetch_add(missing as u64, Ordering::Relaxed);
        }
        n
    }

    /// Samples currently readable.
    pub fn available(&self) -> usize {
        self.consumer.slots()
    }

    /// Total number of samples zero-filled because the ring was empty.
    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }

    /// A shared handle to the underrun counter that stays valid after this source is moved.
    pub fn stats(&self) -> RingStats {
        RingStats {
            counter: Arc::clone(&self.underruns),
        }
    }

    /// Ring capacity in samples (a multiple of [`PcmSource::channels`]).
    pub fn capacity(&self) -> usize {
        self.consumer.buffer().capacity()
    }

    /// Interleaved channels per frame this ring was created for.
    pub fn channels(&self) -> u16 {
        // Created from a `u16` in `pcm_ring_with_channels`.
        u16::try_from(self.channels).unwrap_or(u16::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(n: usize) -> Vec<f32> {
        (0..n).map(|i| i as f32 + 1.0).collect()
    }

    #[test]
    fn stats_outlive_the_moved_ring_ends() {
        let (sink, source) = pcm_ring_with_channels(8, 2);
        let (overruns, underruns) = (sink.stats(), source.stats());
        // Move the ends away (as `CaptureSource::start` / `AudioOutput::start` do) and let the
        // "backend" count glitches.
        let worker = std::thread::spawn(move || {
            let (mut sink, mut source) = (sink, source);
            assert_eq!(sink.push(&ramp(10)), 8); // 2 dropped
            let mut out = [0.0; 12];
            assert_eq!(source.pull(&mut out), 8); // 4 zero-filled
        });
        worker.join().expect("join");
        assert_eq!(overruns.count(), 2);
        assert_eq!(underruns.clone().count(), 4);
    }

    #[test]
    fn capacity_is_rounded_to_whole_frames() {
        let (sink, source) = pcm_ring_with_channels(9, 2);
        assert_eq!(sink.capacity(), 8);
        assert_eq!(source.capacity(), 8);
        assert_eq!((sink.channels(), source.channels()), (2, 2));
        let (sink, _) = pcm_ring_with_channels(1, 6);
        assert_eq!(sink.capacity(), 6, "at least one frame");
        let (sink, _) = pcm_ring(0);
        assert_eq!(sink.capacity(), 1);
        let (sink, _) = pcm_ring_with_channels(10, 0);
        assert_eq!((sink.capacity(), sink.channels()), (10, 1));
    }

    #[test]
    fn push_and_pull_preserve_order_across_wraparound() {
        let (mut sink, mut source) = pcm_ring_with_channels(8, 2);
        let mut next = 0.0f32;
        let mut expected = 0.0f32;
        for _ in 0..50 {
            let block: Vec<f32> = (0..6)
                .map(|_| {
                    next += 1.0;
                    next
                })
                .collect();
            assert_eq!(sink.push(&block), 6);
            assert_eq!(source.available(), 6);
            let mut out = [0.0; 6];
            assert_eq!(source.pull(&mut out), 6);
            for v in out {
                expected += 1.0;
                assert_eq!(v, expected);
            }
        }
        assert_eq!(sink.overruns(), 0);
        assert_eq!(source.underruns(), 0);
    }

    #[test]
    fn push_never_splits_a_frame_and_counts_overruns() {
        // Stereo ring with room for 3 frames.
        let (mut sink, mut source) = pcm_ring_with_channels(6, 2);
        assert_eq!(sink.push(&ramp(4)), 4);
        assert_eq!(sink.free(), 2);
        // 5 samples offered, 2 slots free: exactly one frame is written.
        assert_eq!(sink.push(&[10.0, 11.0, 12.0, 13.0, 14.0]), 2);
        assert_eq!(sink.overruns(), 3);
        // Ring full: everything is dropped.
        assert_eq!(sink.push(&[20.0, 21.0]), 0);
        assert_eq!(sink.overruns(), 5);

        let mut out = [0.0; 6];
        assert_eq!(source.pull(&mut out), 6);
        assert_eq!(out, [1.0, 2.0, 3.0, 4.0, 10.0, 11.0]);

        // Only one free slot in a stereo ring: nothing fits.
        let (mut sink, _source) = pcm_ring_with_channels(4, 2);
        assert_eq!(
            sink.push(&[1.0, 2.0, 3.0]),
            2,
            "trailing partial frame dropped"
        );
        assert_eq!(sink.overruns(), 1);
    }

    #[test]
    fn frames_stay_aligned_under_sustained_overload() {
        let (mut sink, mut source) = pcm_ring_with_channels(10, 2);
        // Left = +frame index, right = -frame index.
        let block: Vec<f32> = (0..7)
            .flat_map(|f| [f as f32 + 1.0, -(f as f32 + 1.0)])
            .collect();
        for _ in 0..20 {
            sink.push(&block);
            let mut out = [0.0; 6];
            let n = source.pull(&mut out);
            assert_eq!(n % 2, 0);
            for frame in out[..n].chunks_exact(2) {
                assert_eq!(frame[0], -frame[1], "channels swapped: {out:?}");
            }
        }
        assert!(sink.overruns() > 0);
        assert_eq!(sink.overruns() % 2, 0, "whole frames dropped");
    }

    #[test]
    fn pull_zero_fills_and_counts_underruns() {
        let (mut sink, mut source) = pcm_ring_with_channels(16, 2);
        assert_eq!(sink.push(&[0.5, -0.5, 0.25, -0.25]), 4);
        let mut out = [9.0; 10];
        assert_eq!(source.pull(&mut out), 4);
        assert_eq!(out, [0.5, -0.5, 0.25, -0.25, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(source.underruns(), 6);
        assert_eq!(source.available(), 0);

        // Odd-length output: the partial trailing frame is zero-filled even with data left.
        sink.push(&ramp(4));
        let mut out = [9.0; 3];
        assert_eq!(source.pull(&mut out), 2);
        assert_eq!(out, [1.0, 2.0, 0.0]);
        assert_eq!(source.underruns(), 7);
        assert_eq!(source.available(), 2);

        // Empty output: nothing happens.
        assert_eq!(source.pull(&mut []), 0);
        assert_eq!(source.underruns(), 7);
    }

    #[test]
    fn works_across_threads() {
        let (mut sink, mut source) = pcm_ring_with_channels(64, 2);
        const FRAMES: usize = 20_000;
        let producer = std::thread::spawn(move || {
            let mut f = 0usize;
            while f < FRAMES {
                let n = (FRAMES - f).min(5);
                let block: Vec<f32> = (f..f + n).flat_map(|i| [i as f32, i as f32]).collect();
                let written = sink.push(&block);
                f += written / 2;
                if written == 0 {
                    std::thread::yield_now();
                }
            }
            sink.overruns()
        });
        let mut got = 0usize;
        let mut out = [0.0f32; 8];
        while got < FRAMES {
            let ready = source.available().min(out.len()) / 2 * 2;
            if ready == 0 {
                std::thread::yield_now();
                continue;
            }
            let n = source.pull(&mut out[..ready]);
            assert_eq!(n, ready);
            for frame in out[..n].chunks_exact(2) {
                assert_eq!(frame[0], got as f32);
                assert_eq!(frame[1], got as f32);
                got += 1;
            }
        }
        // Overruns are expected (the producer retries dropped data): what matters is order.
        let _ = producer.join().expect("join");
    }
}
