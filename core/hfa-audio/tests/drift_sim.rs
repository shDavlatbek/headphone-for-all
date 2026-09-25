//! End-to-end clock-drift simulation: a sender whose clock runs ±300 ppm off the hub's
//! streams 10 ms packets with random arrival jitter for 10 simulated minutes. The hub drives
//! `JitterBuffer` + `DriftController` + `StreamResampler` exactly like the real mixer thread:
//! every 10 ms of hub time it pulls one output frame, popping and resampling packets as
//! needed, then feeds the buffer level to the drift controller.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use hfa_audio::{
    DriftConfig, DriftController, JitterBuffer, JitterConfig, Pop, PushResult, StreamResampler,
};

const FRAME: usize = 480;
const FRAME_US: f64 = 10_000.0;
const FRAME_MS: f64 = 10.0;

/// Deterministic xorshift64* generator (no external RNG dependency).
struct Rng(u64);

impl Rng {
    fn next_f64(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[derive(Debug, Default)]
struct Report {
    underruns_after_start: u64,
    overflows: u64,
    missing: u64,
    /// Largest |1 s average of buffered_ms − target_ms| after convergence.
    worst_avg_error_ms: f64,
    /// Largest instantaneous |buffered_ms − target_ms| after convergence.
    worst_inst_error_ms: f64,
    /// Largest distance, in frames, of the buffered frame count outside the band
    /// `[floor(target) − 1, ceil(target) + 1]` frames (0 = always within ±1 frame).
    frames_outside_band: f64,
    /// Mean ratio deviation (ppm) over the last minute.
    last_minute_ppm: f64,
    /// Standard deviation of the ratio (ppm) after convergence.
    ppm_std: f64,
    min_buffered_ms: f64,
    out_of_order: u64,
    /// `Pop::Stretch` frames over the whole run.
    stretched: u64,
    /// `Pop::Stretch` frames after convergence.
    stretched_after_convergence: u64,
    /// Packets that arrived after their slot was played, concealed or skipped.
    late: u64,
    /// Tick of the last underrun after playout started (0 = none).
    last_underrun_tick: u64,
}

/// Arrival jitter: uniform in `[0, before_us)` until `step_tick`, then `[0, after_us)`.
#[derive(Debug, Clone, Copy)]
struct Jitter {
    before_us: f64,
    step_tick: u64,
    after_us: f64,
    /// `false` emulates a hub that ignores `Pop::Stretch` (pops again instead), as a control.
    honor_stretch: bool,
}

impl Jitter {
    fn constant(max_us: f64) -> Self {
        Self {
            before_us: max_us,
            step_tick: u64::MAX,
            after_us: max_us,
            honor_stretch: true,
        }
    }
}

fn simulate(
    sender_ppm: f64,
    seconds: u64,
    jitter: Jitter,
    seed: u64,
    drift_cfg: DriftConfig,
) -> Report {
    let jcfg = JitterConfig::default();
    let mut jb = JitterBuffer::new(jcfg);
    let mut drift = DriftController::new(drift_cfg);
    let mut rs = StreamResampler::new(1, 48_000, 48_000, FRAME).unwrap();
    let mut rng = Rng(seed);

    // Sender clock: one packet every 10 ms of *sender* time = 10 ms / (1 + ppm) of hub time.
    let send_period_us = FRAME_US / (1.0 + sender_ppm * 1e-6);
    // Start near the u32 limit so seq and timestamp wrap during the run.
    let seq0 = u32::MAX - 20_000;
    let mut next_send = 0_u64; // packet index
    let mut in_flight: BinaryHeap<Reverse<(u64, u64)>> = BinaryHeap::new(); // (arrival_us, idx)

    let mut fifo: Vec<f32> = Vec::with_capacity(FRAME * 4);
    let mut pcm = vec![0.0_f32; FRAME];
    let mut expected_idx: Option<u64> = None;
    let mut report = Report {
        min_buffered_ms: f64::MAX,
        ..Report::default()
    };
    let converge_after_ticks = 120 * 100; // 2 minutes
    let mut started = false;
    let (mut acc_err, mut acc_n) = (0.0, 0);
    let (mut ppm_sum, mut ppm_sq, mut ppm_n) = (0.0, 0.0, 0.0);
    let (mut last_sum, mut last_n) = (0.0, 0.0);

    let ticks = seconds * 100;
    for tick in 1..=ticks {
        let now_us = tick as f64 * FRAME_US;
        // Generate every packet sent before now (arrivals may land later).
        while (next_send as f64) * send_period_us <= now_us {
            let sent = next_send as f64 * send_period_us;
            let max_jitter_us = if tick >= jitter.step_tick {
                jitter.after_us
            } else {
                jitter.before_us
            };
            let arrival = sent + 1_000.0 + rng.next_f64() * max_jitter_us;
            in_flight.push(Reverse((arrival as u64, next_send)));
            next_send += 1;
        }
        // Deliver arrivals up to now.
        while let Some(Reverse((arrival, idx))) = in_flight.peek().copied() {
            if arrival as f64 > now_us {
                break;
            }
            in_flight.pop();
            let seq = seq0.wrapping_add(idx as u32);
            let ts = (idx as u32).wrapping_mul(FRAME as u32).wrapping_add(seq0);
            match jb.push(seq, ts, arrival, idx.to_le_bytes().to_vec()) {
                PushResult::Accepted => {}
                PushResult::Overflow => report.overflows += 1,
                PushResult::TooLate => report.late += 1,
                other => panic!("unexpected push result {other:?} for idx {idx}"),
            }
        }
        // Pull one output frame.
        while fifo.len() < FRAME {
            match jb.pop() {
                Pop::Packet(p) => {
                    let idx = u64::from_le_bytes(p[..8].try_into().unwrap());
                    if let Some(e) = expected_idx {
                        if idx != e {
                            report.out_of_order += 1;
                        }
                    }
                    expected_idx = Some(idx + 1);
                    started = true;
                    pcm.fill(0.1);
                    rs.process(&pcm, &mut fifo).unwrap();
                }
                Pop::Missing { .. } => {
                    report.missing += 1;
                    expected_idx = expected_idx.map(|e| e + 1);
                    pcm.fill(0.0);
                    rs.process(&pcm, &mut fifo).unwrap();
                }
                Pop::Stretch if !jitter.honor_stretch => {}
                Pop::Stretch => {
                    // The hub conceals one frame without consuming a packet.
                    report.stretched += 1;
                    if tick > converge_after_ticks {
                        report.stretched_after_convergence += 1;
                    }
                    pcm.fill(0.1);
                    rs.process(&pcm, &mut fifo).unwrap();
                }
                Pop::Underrun => {
                    if started {
                        report.underruns_after_start += 1;
                        report.last_underrun_tick = tick;
                    }
                    fifo.resize(FRAME, 0.0); // silence for this tick
                }
            }
        }
        fifo.drain(..FRAME);

        let buffered = jb.buffered_ms();
        let target = jb.target_ms();
        if jb.is_primed() {
            let ratio = drift.update(buffered, target, 0.01);
            rs.set_ratio_relative(ratio).unwrap();
        }

        if tick > converge_after_ticks {
            let ppm = drift.ppm();
            ppm_sum += ppm;
            ppm_sq += ppm * ppm;
            ppm_n += 1.0;
            if tick > ticks - 6_000 {
                last_sum += ppm;
                last_n += 1.0;
            }
            let err = buffered - target;
            report.worst_inst_error_ms = report.worst_inst_error_ms.max(err.abs());
            let (b, t) = (buffered / FRAME_MS, target / FRAME_MS);
            let outside = (t.floor() - 1.0 - b).max(b - t.ceil() - 1.0).max(0.0);
            report.frames_outside_band = report.frames_outside_band.max(outside);
            report.min_buffered_ms = report.min_buffered_ms.min(buffered);
            acc_err += err;
            acc_n += 1;
            if acc_n == 100 {
                report.worst_avg_error_ms = report.worst_avg_error_ms.max((acc_err / 100.0).abs());
                acc_err = 0.0;
                acc_n = 0;
            }
        }
    }
    report.last_minute_ppm = last_sum / last_n;
    let mean = ppm_sum / ppm_n;
    report.ppm_std = (ppm_sq / ppm_n - mean * mean).max(0.0).sqrt();
    report
}

/// Runs 10 simulated minutes and checks the steady state. `extra_band_frames` widens the
/// instantaneous ±1 frame band for jitter larger than one frame (the packet count at a tick
/// then legitimately varies by more than one packet).
fn check(sender_ppm: f64, max_jitter_us: f64, extra_band_frames: f64, seed: u64) {
    assert_eq!(f64::from(JitterConfig::default().frame_ms), FRAME_MS);
    let r = simulate(
        sender_ppm,
        600,
        Jitter::constant(max_jitter_us),
        seed,
        DriftConfig::default(),
    );
    eprintln!("{sender_ppm:+} ppm, jitter {max_jitter_us} us: {r:?}");
    assert_eq!(r.underruns_after_start, 0, "underruns: {r:?}");
    assert_eq!(r.overflows, 0, "overflows: {r:?}");
    assert_eq!(r.late, 0, "late packets: {r:?}");
    assert_eq!(r.missing, 0, "missing frames: {r:?}");
    assert_eq!(r.out_of_order, 0, "{r:?}");
    assert_eq!(
        r.stretched_after_convergence, 0,
        "stretching in steady state: {r:?}"
    );
    // Instantaneous level (whole packets) within ±1 frame of the (fractional) target.
    assert!(
        r.frames_outside_band <= extra_band_frames,
        "level left the ±1 frame band: {r:?}"
    );
    // One-second averages track the target closely.
    let avg_limit = FRAME_MS * (1.0 + 0.5 * extra_band_frames);
    assert!(
        r.worst_avg_error_ms <= avg_limit,
        "average level off target: {r:?}"
    );
    // The controller settled on the clock offset (a faster sender needs ratio < 1).
    assert!(
        (r.last_minute_ppm + sender_ppm).abs() < 30.0,
        "ratio: {r:?}"
    );
}

#[test]
fn drift_plus_300_ppm_for_10_minutes() {
    check(300.0, 8_000.0, 0.0, 0x9E37_79B9_7F4A_7C15);
}

#[test]
fn drift_minus_300_ppm_for_10_minutes() {
    check(-300.0, 8_000.0, 0.0, 0xD1B5_4A32_D192_ED03);
}

/// Wi-Fi-like jitter up to 25 ms (packets arrive reordered); the adaptive target grows
/// above its minimum and the instantaneous level may stray one more frame.
#[test]
fn drift_plus_300_ppm_with_reordering_jitter() {
    check(300.0, 25_000.0, 1.0, 0x1234_5678_9ABC_DEF1);
}

/// See [`drift_plus_300_ppm_with_reordering_jitter`].
#[test]
fn drift_minus_300_ppm_with_reordering_jitter() {
    check(-300.0, 25_000.0, 1.0, 0x0FED_CBA9_8765_4321);
}

/// Control experiment: with drift correction disabled the same setup fails, so the tests
/// above really exercise the controller.
#[test]
fn without_correction_the_buffer_drifts_away() {
    let off = DriftConfig {
        max_ppm: 0.0,
        ..DriftConfig::default()
    };
    let slow = simulate(-300.0, 300, Jitter::constant(8_000.0), 7, off);
    // The draining buffer runs dry (the target is too low for stretching to kick in).
    assert!(slow.underruns_after_start > 0, "{slow:?}");
    let fast = simulate(300.0, 300, Jitter::constant(8_000.0), 8, off);
    assert!(fast.worst_avg_error_ms > 40.0, "{fast:?}");
}

/// A sudden jitter increase (2 ms → 40 ms after 30 s) raises the target at once. The first
/// late packets right at the step cannot be absorbed by any buffer sized for 2 ms of jitter,
/// but stretch frames then grow the buffer to the new target within about a second (the
/// drift controller alone manages ≤ 2 ms/s), so playout does not keep running dry or
/// concealing late packets. Control: the same runs with the stretch requests ignored.
#[test]
fn jitter_step_is_followed_by_stretching_without_repeated_underruns() {
    let step_tick = 30 * 100;
    let jitter = Jitter {
        before_us: 2_000.0,
        step_tick,
        after_us: 40_000.0,
        honor_stretch: true,
    };
    let seeds = [
        0xA5A5_5A5A_1234_4321,
        0x0BAD_F00D_DEAD_BEEF,
        0x7777_1234_ABCD_0001,
        0x5555_AAAA_0000_FFFF,
    ];
    // Audible glitches: underruns plus frames concealed because their packet came too late.
    let glitches = |r: &Report| r.underruns_after_start + r.missing;
    let (mut total, mut control_total) = (0, 0);
    for seed in seeds {
        let r = simulate(300.0, 45, jitter, seed, DriftConfig::default());
        eprintln!("jitter step: {r:?}");
        // (A few packets delayed past their slot right at the step are concealed: `missing`.)
        assert_eq!(r.out_of_order, 0, "{r:?}");
        assert!(r.stretched > 0, "{r:?}");
        assert!(
            r.last_underrun_tick < step_tick + 200,
            "underruns continue 2 s after the step: {r:?}"
        );
        total += glitches(&r);
        // Control: a hub that ignores stretch requests.
        let control = simulate(
            300.0,
            45,
            Jitter {
                honor_stretch: false,
                ..jitter
            },
            seed,
            DriftConfig::default(),
        );
        eprintln!("jitter step, stretch ignored: {control:?}");
        control_total += glitches(&control);
    }
    assert!(
        3 * total < control_total,
        "glitches with stretching: {total}, without: {control_total}"
    );
}
