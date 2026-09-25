//! Signal analysis of the selftest's output WAV: per-tone amplitude (Hann-windowed Goertzel
//! over short blocks), glitch detection, dropouts and onset detection.
//!
//! Short blocks keep every measure robust to what a real playout does to a tone: the drift
//! controller resamples by up to ±2000 ppm and concealment or buffer stretching shifts the
//! phase, which would partly cancel one long Goertzel sum.

/// Sample rate of the analysed signal (the hub's WAV output is [`hfa_audio::AudioFormat::INTERNAL`]).
pub const RATE: f64 = 48_000.0;
/// Analysis window of the tone tracker and glitch detector.
pub const BLOCK_S: f64 = 0.020;
/// Hop between two analysis windows.
pub const HOP_S: f64 = 0.010;
/// A window is a glitch when a tone's amplitude leaves `[LOW, HIGH] × its median`...
pub const GLITCH_LOW: f64 = 0.5;
/// ...see [`GLITCH_LOW`].
pub const GLITCH_HIGH: f64 = 1.5;
/// ...or when the energy that is not one of the tones exceeds this fraction of the tones'
/// energy (−17 dB: clicks, splatter of a phase jump, noise bursts; clean playout stays
/// below −25 dB).
pub const GLITCH_RESIDUAL: f64 = 0.02;
/// RMS below which a 5 ms block counts as a dropout (−34 dBFS).
pub const DROPOUT_RMS: f64 = 0.02;

/// Sample index of `t` seconds (clamped at 0).
pub fn index(t: f64) -> usize {
    (t.max(0.0) * RATE).round() as usize
}

/// `x[t0..t1]` in seconds, clamped to the signal.
pub fn slice(x: &[f32], t0: f64, t1: f64) -> &[f32] {
    let a = index(t0).min(x.len());
    let b = index(t1).min(x.len()).max(a);
    &x[a..b]
}

/// Reads the left channel of `[t0, t1)` seconds of a 48 kHz stereo 32-bit float WAV (the
/// hub's output, [`hfa_audio::AudioFormat::INTERNAL`]) without loading the rest of the file.
/// The range is clamped to the file.
///
/// # Errors
/// The file cannot be read or has another format.
pub fn read_left(path: &std::path::Path, t0: f64, t1: f64) -> anyhow::Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path)?;
    let spec = reader.spec();
    let expected = hfa_audio::AudioFormat::INTERNAL;
    if spec.sample_rate != expected.sample_rate
        || spec.channels != expected.channels
        || spec.sample_format != hound::SampleFormat::Float
        || spec.bits_per_sample != 32
    {
        anyhow::bail!(
            "unexpected WAV format: {} Hz, {} channel(s), {:?} {} bits (want {} Hz stereo f32)",
            spec.sample_rate,
            spec.channels,
            spec.sample_format,
            spec.bits_per_sample,
            expected.sample_rate
        );
    }
    let frames = reader.duration() as usize;
    let a = index(t0).min(frames);
    let b = index(t1).min(frames).max(a);
    reader.seek(u32::try_from(a)?)?;
    let left = reader
        .samples::<f32>()
        .take(2 * (b - a))
        .step_by(2)
        .collect::<Result<Vec<f32>, _>>()?;
    Ok(left)
}

/// Precomputed Hann window of `len` samples and its sum.
#[derive(Debug, Clone)]
pub struct Hann {
    w: Vec<f64>,
    sum: f64,
    sum_sq: f64,
}

impl Hann {
    /// A Hann window of `len` samples (at least 2).
    pub fn new(len: usize) -> Self {
        let len = len.max(2);
        let w: Vec<f64> = (0..len)
            .map(|n| 0.5 - 0.5 * (std::f64::consts::TAU * n as f64 / (len - 1) as f64).cos())
            .collect();
        let sum = w.iter().sum();
        let sum_sq = w.iter().map(|v| v * v).sum();
        Self { w, sum, sum_sq }
    }

    /// Window length in samples.
    pub fn len(&self) -> usize {
        self.w.len()
    }

    /// Amplitude of the `freq` component of `x[..len]` (a sine of amplitude `A` gives ≈ `A`).
    pub fn amplitude(&self, x: &[f32], freq: f64) -> f64 {
        let coeff = 2.0 * (std::f64::consts::TAU * freq / RATE).cos();
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for (v, w) in x.iter().zip(&self.w) {
            let s0 = f64::from(*v) * w + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        let power = (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0);
        2.0 * power.sqrt() / self.sum
    }

    /// Windowed mean square of `x[..len]` (a sine of amplitude `A` gives ≈ `A²/2`).
    pub fn mean_square(&self, x: &[f32]) -> f64 {
        x.iter()
            .zip(&self.w)
            .map(|(v, w)| (f64::from(*v) * w).powi(2))
            .sum::<f64>()
            / self.sum_sq
    }
}

/// Median of `v` (0 for an empty slice).
pub fn median(v: &[f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    s[s.len() / 2]
}

/// Amplitude of each tone in every analysis window (`[tone][window]`).
pub fn track(x: &[f32], freqs: &[f64]) -> Vec<Vec<f64>> {
    let hann = Hann::new(index(BLOCK_S));
    let hop = index(HOP_S);
    let starts: Vec<usize> = (0..)
        .map(|k| k * hop)
        .take_while(|s| s + hann.len() <= x.len())
        .collect();
    freqs
        .iter()
        .map(|f| {
            starts
                .iter()
                .map(|&s| hann.amplitude(&x[s..s + hann.len()], *f))
                .collect()
        })
        .collect()
}

/// Glitches found by [`glitches`].
#[derive(Debug, Clone, PartialEq)]
pub struct GlitchReport {
    /// Median amplitude of each tone over the signal.
    pub amplitudes: Vec<f64>,
    /// Glitch events (runs of consecutive abnormal windows).
    pub events: usize,
    /// Abnormal windows.
    pub bad_windows: usize,
    /// Analysis windows.
    pub windows: usize,
    /// Start times (s, relative to `x`) of the first few events.
    pub first_events: Vec<f64>,
}

/// Finds glitches in a steady mix of the tones `freqs`: windows where a tone's amplitude
/// leaves `[GLITCH_LOW, GLITCH_HIGH] ×` its median, or where energy that is not one of the
/// tones exceeds [`GLITCH_RESIDUAL`] of the tones' energy. Runs of consecutive abnormal
/// windows count as one event.
pub fn glitches(x: &[f32], freqs: &[f64]) -> GlitchReport {
    let hann = Hann::new(index(BLOCK_S));
    let hop = index(HOP_S);
    let tracks = track(x, freqs);
    let amplitudes: Vec<f64> = tracks.iter().map(|t| median(t)).collect();
    let tone_energy: f64 = amplitudes.iter().map(|a| a * a / 2.0).sum();
    let windows = tracks.first().map_or(0, Vec::len);
    let (mut events, mut bad_windows, mut in_event) = (0, 0, false);
    let mut first_events = Vec::new();
    for w in 0..windows {
        let block = &x[w * hop..w * hop + hann.len()];
        let amps: Vec<f64> = tracks.iter().map(|t| t[w]).collect();
        let level_bad = amps
            .iter()
            .zip(&amplitudes)
            .any(|(a, m)| *a < GLITCH_LOW * m || *a > GLITCH_HIGH * m);
        let explained: f64 = amps.iter().map(|a| a * a / 2.0).sum();
        let residual = hann.mean_square(block) - explained;
        let bad = level_bad || residual > GLITCH_RESIDUAL * tone_energy;
        if bad {
            bad_windows += 1;
            if !in_event {
                events += 1;
                if first_events.len() < 8 {
                    first_events.push(w as f64 * HOP_S);
                }
            }
        }
        in_event = bad;
    }
    GlitchReport {
        amplitudes,
        events,
        bad_windows,
        windows,
        first_events,
    }
}

/// Longest run of consecutive 5 ms blocks with an RMS below [`DROPOUT_RMS`], in ms.
pub fn longest_dropout_ms(x: &[f32]) -> f64 {
    let block = index(0.005);
    let (mut run, mut longest) = (0usize, 0usize);
    for chunk in x.chunks(block) {
        let ms = chunk.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>() / chunk.len() as f64;
        if ms.sqrt() < DROPOUT_RMS {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    longest as f64 * 5.0
}

/// Time (s, relative to `x`) of the onset of a `freq` tone of steady amplitude `reference`:
/// the centre of the first 10 ms window (1 ms steps) whose amplitude reaches half of
/// `reference`. For a step onset that is the onset itself (±1 ms).
///
/// `None` if no window reaches it, or if `reference` is not positive (a missing tone has no
/// onset: half of zero would match the first window of silence).
pub fn onset(x: &[f32], freq: f64, reference: f64) -> Option<f64> {
    if reference.is_nan() || reference <= 0.0 {
        return None;
    }
    let hann = Hann::new(index(0.010));
    let step = index(0.001);
    (0..)
        .map(|k| k * step)
        .take_while(|s| s + hann.len() <= x.len())
        .find(|&s| hann.amplitude(&x[s..s + hann.len()], freq) >= 0.5 * reference)
        .map(|s| (s as f64 + hann.len() as f64 / 2.0) / RATE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tones(seconds: f64, parts: &[(f64, f32)]) -> Vec<f32> {
        (0..index(seconds))
            .map(|n| {
                parts
                    .iter()
                    .map(|(f, a)| a * (std::f64::consts::TAU * f * n as f64 / RATE).sin() as f32)
                    .sum()
            })
            .collect()
    }

    #[test]
    fn measures_tone_amplitudes() {
        let x = tones(1.0, &[(440.0, 0.25), (1000.0, 0.1)]);
        let tracks = track(&x, &[440.0, 1000.0, 2500.0]);
        let (a440, a1000, a2500) = (median(&tracks[0]), median(&tracks[1]), median(&tracks[2]));
        assert!((a440 - 0.25).abs() < 0.01, "{a440}");
        assert!((a1000 - 0.1).abs() < 0.005, "{a1000}");
        assert!(a2500 < 0.002, "{a2500}");
        let hann = Hann::new(960);
        let ms = hann.mean_square(&x[..960]);
        assert!(
            (ms - (0.25f64.powi(2) + 0.1f64.powi(2)) / 2.0).abs() < 0.002,
            "{ms}"
        );
    }

    #[test]
    fn a_clean_mix_has_no_glitches_even_with_drift() {
        // 2000 ppm off the nominal frequency (the drift controller's limit).
        let x = tones(2.0, &[(440.0 * 1.002, 0.25), (1000.0 * 0.998, 0.25)]);
        let r = glitches(&x, &[440.0, 1000.0]);
        assert_eq!(r.events, 0, "{r:?}");
        assert!(r.windows > 190);
        assert_eq!(longest_dropout_ms(&x), 0.0);
    }

    #[test]
    fn detects_gaps_clicks_and_phase_jumps() {
        let mut x = tones(2.0, &[(440.0, 0.25), (1000.0, 0.25)]);
        // A 10 ms gap at 0.5 s.
        x[index(0.5)..index(0.51)].fill(0.0);
        // A click at 1.0 s.
        x[index(1.0)] = 0.9;
        x[index(1.0) + 1] = -0.9;
        // A phase jump (half a period of 440 Hz) at 1.5 s.
        let jump = index(1.5);
        let shifted = tones(2.0, &[(440.0, 0.25), (1000.0, 0.25)]);
        let half = index(0.5 / 440.0);
        let end = x.len() - half;
        x[jump..end].copy_from_slice(&shifted[jump + half..]);
        let r = glitches(&x, &[440.0, 1000.0]);
        assert_eq!(r.events, 3, "{r:?}");
        assert!((r.first_events[0] - 0.49).abs() < 0.03, "{r:?}");
        assert!(longest_dropout_ms(&x) >= 5.0);
    }

    #[test]
    fn finds_a_step_onset_among_other_tones() {
        let background = tones(1.0, &[(1000.0, 0.25), (2500.0, 0.25)]);
        let tone = tones(1.0, &[(440.0, 0.25)]);
        let start = index(0.3137);
        let x: Vec<f32> = background
            .iter()
            .enumerate()
            .map(|(n, b)| b + if n >= start { tone[n - start] } else { 0.0 })
            .collect();
        let t = onset(&x, 440.0, 0.25).expect("onset");
        assert!((t - 0.3137).abs() < 0.0015, "{t}");
        assert!(onset(&background, 440.0, 0.25).is_none());
    }

    #[test]
    fn a_missing_tone_has_no_onset() {
        // With a zero reference every window would "reach half of it".
        let silence = vec![0.0f32; index(0.5)];
        assert!(onset(&silence, 440.0, 0.0).is_none());
        assert!(onset(&silence, 440.0, f64::NAN).is_none());
    }

    #[test]
    fn reads_only_the_left_channel_of_a_range() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mix.wav");
        let fmt = hfa_audio::AudioFormat::INTERNAL;
        let mut w = hfa_audio::wav::WavWriter::create(&path, fmt).unwrap();
        // Left = frame index / 1e6, right = -1.
        let frames = index(1.0);
        let data: Vec<f32> = (0..frames).flat_map(|n| [n as f32 / 1e6, -1.0]).collect();
        w.write(&data).unwrap();
        w.finalize().unwrap();

        let left = read_left(&path, 0.25, 0.5).unwrap();
        assert_eq!(left.len(), index(0.25));
        assert_eq!(left[0], index(0.25) as f32 / 1e6);
        assert!(left.iter().all(|v| *v >= 0.0), "no right-channel samples");
        // Clamped to the file.
        assert_eq!(
            read_left(&path, 0.9, 5.0).unwrap().len(),
            frames - index(0.9)
        );
        assert!(read_left(&path, 2.0, 3.0).unwrap().is_empty());

        let mono = dir.path().join("mono.wav");
        let mut w =
            hfa_audio::wav::WavWriter::create(&mono, hfa_audio::AudioFormat::new(48_000, 1))
                .unwrap();
        w.write(&[0.0; 480]).unwrap();
        w.finalize().unwrap();
        assert!(read_left(&mono, 0.0, 1.0).is_err());
    }

    #[test]
    fn slices_are_clamped() {
        let x = vec![0.0f32; 480];
        assert_eq!(slice(&x, 0.005, 1.0).len(), 240);
        assert!(slice(&x, 2.0, 3.0).is_empty());
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[]), 0.0);
    }
}
