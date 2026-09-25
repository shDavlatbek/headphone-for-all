//! Lock-free SPSC PCM ring (`rtrb`) connecting audio callbacks with the rest of the program.
//! Both ends are real-time safe: no allocation, no locks, no syscalls.

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

/// Creates a ring holding up to `capacity_samples` interleaved `f32` samples.
pub fn pcm_ring(_capacity_samples: usize) -> (PcmSink, PcmSource) {
    todo!("feat/capture")
}

/// Producer end (written by capture callbacks / the hub mixer thread).
pub struct PcmSink {
    producer: rtrb::Producer<f32>,
    overruns: Arc<AtomicU64>,
}

impl PcmSink {
    /// Writes as many samples as fit and returns how many were written. Samples that do not
    /// fit are dropped and counted in [`PcmSink::overruns`].
    pub fn push(&mut self, _interleaved: &[f32]) -> usize {
        let _ = (&self.producer, &self.overruns);
        todo!("feat/capture")
    }

    /// Total number of samples dropped because the ring was full.
    pub fn overruns(&self) -> u64 {
        todo!("feat/capture")
    }

    /// A shared handle to the overrun counter that stays valid after this sink is moved.
    pub fn stats(&self) -> RingStats {
        RingStats {
            counter: Arc::clone(&self.overruns),
        }
    }

    /// Free space in samples.
    pub fn free(&self) -> usize {
        todo!("feat/capture")
    }

    /// Ring capacity in samples.
    pub fn capacity(&self) -> usize {
        todo!("feat/capture")
    }
}

/// Consumer end (read by output callbacks / the sender encoder thread).
pub struct PcmSource {
    consumer: rtrb::Consumer<f32>,
    underruns: Arc<AtomicU64>,
}

impl PcmSource {
    /// Fills `out` with up to `out.len()` samples and returns how many were real; the rest of
    /// `out` is zero-filled and counted in [`PcmSource::underruns`].
    pub fn pull(&mut self, _out: &mut [f32]) -> usize {
        let _ = (&self.consumer, &self.underruns);
        todo!("feat/capture")
    }

    /// Samples currently readable.
    pub fn available(&self) -> usize {
        todo!("feat/capture")
    }

    /// Total number of samples zero-filled because the ring was empty.
    pub fn underruns(&self) -> u64 {
        todo!("feat/capture")
    }

    /// A shared handle to the underrun counter that stays valid after this source is moved.
    pub fn stats(&self) -> RingStats {
        RingStats {
            counter: Arc::clone(&self.underruns),
        }
    }

    /// Ring capacity in samples.
    pub fn capacity(&self) -> usize {
        todo!("feat/capture")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_outlive_the_moved_ring_ends() {
        let (producer, consumer) = rtrb::RingBuffer::<f32>::new(8);
        let sink = PcmSink {
            producer,
            overruns: Arc::new(AtomicU64::new(0)),
        };
        let source = PcmSource {
            consumer,
            underruns: Arc::new(AtomicU64::new(0)),
        };
        let (overruns, underruns) = (sink.stats(), source.stats());
        // Move the ends away (as `CaptureSource::start` / `AudioOutput::start` do) and let the
        // "backend" count glitches.
        let worker = std::thread::spawn(move || {
            sink.overruns.fetch_add(3, Ordering::Relaxed);
            source.underruns.fetch_add(5, Ordering::Relaxed);
        });
        worker.join().expect("join");
        assert_eq!(overruns.count(), 3);
        assert_eq!(underruns.clone().count(), 5);
    }
}
