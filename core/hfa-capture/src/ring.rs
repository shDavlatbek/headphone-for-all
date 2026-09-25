//! Lock-free SPSC PCM ring (`rtrb`) connecting audio callbacks with the rest of the program.
//! Both ends are real-time safe: no allocation, no locks, no syscalls.

use std::sync::atomic::AtomicU64;
use std::sync::Arc;

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

    /// Ring capacity in samples.
    pub fn capacity(&self) -> usize {
        todo!("feat/capture")
    }
}
