//! Verifies the real-time paths allocate nothing after warm-up, using a counting global
//! allocator that only tracks the current thread while a probe is active.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use hfa_audio::{
    AudioFormat, DriftConfig, DriftController, JitterBuffer, JitterConfig, Mixer, MixerConfig, Pop,
    SineGenerator, StreamResampler,
};

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static TRACKING: Cell<bool> = const { Cell::new(false) };
}

fn note() {
    if TRACKING.try_with(|t| t.get()).unwrap_or(false) {
        ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
    }
}

// SAFETY: forwards to the system allocator; only adds a counter.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Runs `f` and returns how many allocations it made on this thread.
fn count_allocs(f: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.load(Ordering::SeqCst);
    TRACKING.with(|t| t.set(true));
    f();
    TRACKING.with(|t| t.set(false));
    ALLOCATIONS.load(Ordering::SeqCst) - before
}

#[test]
fn mixer_mix_does_not_allocate() {
    const FRAMES: usize = 480;
    let mut m = Mixer::new(MixerConfig::default(), FRAMES);
    for id in 0..8 {
        m.add_source(id);
    }
    m.set_priority(3, true);
    let mut gen = SineGenerator::new(440.0, 0.9, AudioFormat::INTERNAL);
    let mut bufs = vec![vec![0.0_f32; FRAMES * 2]; 8];
    for b in &mut bufs {
        gen.fill(b);
    }
    let inputs: Vec<(u32, &[f32])> = bufs
        .iter()
        .enumerate()
        .map(|(i, b)| (i as u32, b.as_slice()))
        .collect();
    let mut out = vec![0.0_f32; FRAMES * 2];
    m.mix(&inputs, &mut out);
    let n = count_allocs(|| {
        for i in 0..100 {
            m.set_gain(i % 8, 0.5 + (i % 3) as f32);
            m.set_muted(5, i % 2 == 0);
            m.set_master_gain(1.5);
            m.mix(&inputs, &mut out);
            let _ = m.master_level();
        }
    });
    assert_eq!(n, 0, "Mixer::mix allocated {n} times");
}

#[test]
fn hub_stream_path_does_not_allocate_after_warmup() {
    const FRAMES: usize = 480;
    let mut jb = JitterBuffer::new(JitterConfig::default());
    let mut drift = DriftController::new(DriftConfig::default());
    let mut rs = StreamResampler::new(2, 48_000, 48_000, FRAMES).unwrap();
    let pcm = vec![0.1_f32; FRAMES * 2];
    let mut fifo: Vec<f32> = Vec::with_capacity(FRAMES * 8);
    // Payloads are allocated by the network thread, outside the probe.
    let payloads: Vec<Vec<u8>> = (0..400).map(|i: u32| i.to_le_bytes().to_vec()).collect();
    let mut payloads = payloads.into_iter();
    let mut total = 0;
    for seq in 0..300_u32 {
        let payload = payloads.next().unwrap();
        let probe = seq >= 100;
        let n = count_allocs(|| {
            jb.push(seq, seq * 480, u64::from(seq) * 10_000, payload);
            if let Pop::Packet(p) = jb.pop() {
                drop(p);
                rs.process(&pcm, &mut fifo).unwrap();
            }
            let keep = fifo.len().saturating_sub(FRAMES * 2);
            fifo.drain(..fifo.len() - keep);
            let r = drift.update(jb.buffered_ms(), jb.target_ms(), 0.01);
            rs.set_ratio_relative(r).unwrap();
        });
        if probe {
            total += n;
        }
    }
    assert_eq!(total, 0, "hub stream path allocated {total} times");
}

#[test]
fn probe_detects_allocations() {
    let n = count_allocs(|| {
        let v = std::hint::black_box(vec![1_u8; 16]);
        drop(v);
    });
    assert_eq!(n, 1);
}
