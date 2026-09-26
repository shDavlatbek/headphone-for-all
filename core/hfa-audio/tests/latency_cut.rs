//! Latency cuts through the hub's per-stream recipe (docs/CONTRACTS.md §4): the jitter buffer
//! hands every discarded slot back as `Pop::Skipped`, the consumer still decodes it, so the
//! Opus decoder state stays continuous and every frame after a cut decodes exactly as if
//! nothing had been cut; the `Splicer` joins the audio without a click.

use hfa_audio::{
    AudioFormat, JitterBuffer, JitterConfig, OpusConfig, OpusDecoder, OpusEncoder, Pop,
    SineGenerator, Splicer,
};

const FRAME: usize = 480;

/// `n` 10 ms Opus packets of a steady 440 Hz tone, each prefixed with its index (4 bytes BE)
/// so the test knows which one the jitter buffer returns.
fn packets(n: usize) -> Vec<Vec<u8>> {
    let mut enc = OpusEncoder::new(OpusConfig::default()).expect("encoder");
    let mut tone = SineGenerator::new(440.0, 0.25, AudioFormat::INTERNAL);
    let mut pcm = vec![0.0; FRAME * 2];
    (0..n as u32)
        .map(|i| {
            tone.fill(&mut pcm);
            let mut buf = vec![0u8; 1275];
            let len = enc.encode(&pcm, &mut buf).expect("encode");
            let mut p = i.to_be_bytes().to_vec();
            p.extend_from_slice(&buf[..len]);
            p
        })
        .collect()
}

fn index(p: &[u8]) -> usize {
    u32::from_be_bytes([p[0], p[1], p[2], p[3]]) as usize
}

fn decode(dec: &mut OpusDecoder, p: &[u8]) -> Vec<f32> {
    let mut out = vec![0.0; FRAME * 2];
    let frames = dec.decode(&p[4..], &mut out).expect("decode");
    assert_eq!(frames, FRAME);
    out
}

fn max_diff(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0, f32::max)
}

/// What [`play`] produced.
struct Run {
    /// Frames the recipe's decoder played: (packet index, pcm).
    played: Vec<(usize, Vec<f32>)>,
    /// The same frames from a decoder that never saw the discarded packets (the old
    /// behaviour).
    naive: Vec<Vec<f32>>,
    /// The spliced output, left channel.
    out: Vec<f32>,
    /// Slots the jitter buffer discarded.
    skipped: u64,
}

/// Runs `pkts` through a jitter buffer, one frame played per 10 ms tick: one packet arrives
/// per tick, except at tick 50 where `burst` packets arrive at once (far more than the target,
/// like the burst after a network stall).
fn play(pkts: &[Vec<u8>], burst: usize) -> Run {
    let mut jb = JitterBuffer::new(JitterConfig {
        frame_ms: 10,
        min_target_ms: 20,
        max_target_ms: 150,
        initial_target_ms: 20,
        capacity: 64,
    });
    let mut recipe = OpusDecoder::new(48_000, 2).expect("decoder");
    let mut naive = OpusDecoder::new(48_000, 2).expect("decoder");
    let mut splicer = Splicer::new(48_000, 2);
    let (mut played, mut naive_played, mut out) = (Vec::new(), Vec::new(), Vec::new());
    let push = |jb: &mut JitterBuffer, i: usize, tick: usize| {
        jb.push(
            i as u32,
            (i * FRAME) as u32,
            tick as u64 * 10_000,
            pkts[i].clone(),
        );
    };
    let mut next = 0;
    for tick in 0.. {
        let arriving = if tick == 50 { burst } else { 1 };
        if next + arriving > pkts.len() {
            break;
        }
        for _ in 0..arriving {
            push(&mut jb, next, tick);
            next += 1;
        }
        loop {
            match jb.pop() {
                Pop::Skipped(Some(p)) => {
                    // Decoded all the same, not played.
                    let pcm = decode(&mut recipe, &p);
                    splicer.discard(&pcm);
                }
                Pop::Skipped(None) => panic!("nothing is lost"),
                Pop::Packet(p) => {
                    let pcm = decode(&mut recipe, &p);
                    naive_played.push(decode(&mut naive, &p));
                    let (head, rest) = splicer.splice(&pcm);
                    out.extend(head.iter().chain(rest).step_by(2));
                    played.push((index(&p), pcm));
                    break;
                }
                Pop::Underrun => break,
                other => panic!("{other:?}"),
            }
        }
    }
    Run {
        played,
        naive: naive_played,
        out,
        skipped: jb.stats().skipped,
    }
}

#[test]
fn every_frame_after_a_cut_decodes_as_without_the_cut() {
    let pkts = packets(300);
    // Reference: every packet decoded in order.
    let mut reference = OpusDecoder::new(48_000, 2).expect("decoder");
    let expected: Vec<Vec<f32>> = pkts.iter().map(|p| decode(&mut reference, p)).collect();
    // 60 packets at once: one cut of ~40 frames. 15 at once: single-frame cuts, 100 ms apart.
    for burst in [60, 15] {
        let Run {
            played,
            naive,
            out,
            skipped,
        } = play(&pkts, burst);
        assert!(skipped > 5, "burst {burst}: {skipped} discarded");
        let mut first_after_cut = Vec::new();
        for (k, (i, pcm)) in played.iter().enumerate() {
            // Bit-exact: the decoder never saw a gap.
            assert_eq!(pcm, &expected[*i], "burst {burst}: frame {i}");
            if k > 0 && played[k - 1].0 + 1 != *i {
                first_after_cut.push(k);
            }
        }
        assert!(!first_after_cut.is_empty());
        // The control: a decoder that never saw the discarded packets starts every frame after
        // a cut from the wrong state (its overlap and prediction belong to the frame before).
        let worst = first_after_cut
            .iter()
            .map(|&k| max_diff(&naive[k], &expected[played[k].0]))
            .fold(0.0, f32::max);
        assert!(
            worst > 0.01,
            "burst {burst}: naive decode differs by {worst} only"
        );
        // And the spliced output has no step larger than the tone's own (plus codec noise).
        let own = (std::f64::consts::TAU * 440.0 / 48_000.0).sin() as f32 * 0.25;
        let step = out
            .windows(2)
            .skip(FRAME * 3)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max);
        assert!(step < own * 1.25, "burst {burst}: step {step} (tone {own})");
    }
}
