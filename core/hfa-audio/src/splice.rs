//! Inaudible latency cuts: splices a decoded stream around the frames a jitter buffer
//! discarded ([`crate::Pop::Skipped`]).
//!
//! Jumping from the last frame played before a cut straight to the first one after it is
//! audible: the waveform steps, and a periodic sound jumps in phase (a 10 ms cut shifts a
//! 440 Hz tone by 144°, a click plus a dip of that tone). The [`Splicer`] makes the cut
//! seamless, WSOLA style:
//!
//! 1. The caller still **decodes every discarded packet** (so the decoder state stays
//!    continuous; Opus predicts each frame from the previous one) and hands that audio to
//!    [`Splicer::discard`] instead of playing it. The splicer keeps the first
//!    [`CROSSFADE_MS`] of the first discarded frame, the **tail** (what the listener would
//!    have heard next), and the last [`SEARCH_MS`] of the last one.
//! 2. The first frame played after the cut goes through [`Splicer::splice`]. It looks for
//!    the splice point within `±SEARCH_MS` of the nominal cut where the audio after the cut
//!    best matches the tail (normalized cross-correlation of the channel sum; among equally
//!    good points the one closest to the nominal cut wins), so a periodic sound goes on in
//!    phase.
//! 3. It crossfades from the tail into the audio at that point over [`CROSSFADE_MS`] with an
//!    **equal-power fade compensated for the measured correlation** `c`: gains `a = cos θ / n`,
//!    `b = sin θ / n` with `n = √(1 + c·sin 2θ)`, so `a² + b² + 2·c·a·b = 1`. For unrelated
//!    audio (`c = 0`) that is the classic equal-power fade; for matching audio (`c = 1`) it
//!    becomes an equal-gain fade (`a + b = 1`), which neither dips nor bumps the level.
//!
//! The cut is therefore the discarded frames ± up to [`SEARCH_MS`] (the difference leaves the
//! next frame slightly shorter or longer; a consumer with a fixed-chunk resampler keeps it as a
//! partial chunk). Nothing is allocated after [`Splicer::new`]: the mixer thread may call every
//! method.

/// Length of the crossfade at a splice, in ms.
pub const CROSSFADE_MS: u32 = 5;

/// The splice point is searched within this many ms before and after the nominal cut (a
/// search span of twice this covers one period of every pitch down to 100 Hz).
pub const SEARCH_MS: u32 = 5;

/// Score penalty for the farthest splice point of the search: among splice points that match
/// about equally well, the one nearest the nominal cut is chosen.
const OFFSET_PENALTY: f64 = 0.05;

/// Splices a decoded stream around discarded frames (see the module docs). One per stream.
#[derive(Debug, Clone)]
pub struct Splicer {
    channels: usize,
    /// Crossfade length in frames.
    crossfade: usize,
    /// Search half-width in frames.
    search: usize,
    /// First (up to `crossfade`) frames of the discarded audio, interleaved.
    tail: Vec<f32>,
    /// Last (up to `search`) frames of the discarded audio, interleaved.
    last: Vec<f32>,
    /// Frames were discarded since the last splice: the next played frame is spliced.
    pending: bool,
    /// Channel sums of `tail` and of the candidate audio after the cut (scratch).
    tail_sum: Vec<f64>,
    post_sum: Vec<f64>,
    /// The spliced start of the frame: crossfade + rest of `last` (scratch).
    head: Vec<f32>,
    /// Splices made so far.
    splices: u64,
}

impl Splicer {
    /// A splicer for interleaved audio with `channels` channels at `sample_rate` Hz.
    pub fn new(sample_rate: u32, channels: u16) -> Self {
        let channels = usize::from(channels.max(1));
        let frames = |ms: u32| (sample_rate as usize * ms as usize / 1000).max(1);
        let crossfade = frames(CROSSFADE_MS);
        let search = frames(SEARCH_MS);
        Self {
            channels,
            crossfade,
            search,
            tail: Vec::with_capacity(crossfade * channels),
            last: Vec::with_capacity(search * channels),
            pending: false,
            tail_sum: Vec::with_capacity(crossfade),
            post_sum: Vec::with_capacity(2 * search + crossfade),
            head: Vec::with_capacity((crossfade + search) * channels),
            splices: 0,
        }
    }

    /// `true` while discarded audio waits for the next played frame.
    pub fn is_pending(&self) -> bool {
        self.pending
    }

    /// Splices made so far.
    pub fn splices(&self) -> u64 {
        self.splices
    }

    /// Crossfade length in frames.
    pub fn crossfade_frames(&self) -> usize {
        self.crossfade
    }

    /// Forgets discarded audio (a stream reset or an underrun: there is nothing to splice
    /// onto).
    pub fn reset(&mut self) {
        self.pending = false;
        self.tail.clear();
        self.last.clear();
    }

    /// Audio of a discarded frame (decoded, or concealed for a slot that never arrived), in
    /// stream order: it is not played, but its start becomes the tail the next played frame
    /// is crossfaded from.
    pub fn discard(&mut self, pcm: &[f32]) {
        let ch = self.channels;
        let pcm = &pcm[..pcm.len() / ch * ch];
        if !self.pending {
            self.pending = true;
            self.tail.clear();
            self.last.clear();
        }
        // The tail: the first `crossfade` frames after the last played one.
        let missing = self.crossfade * ch - self.tail.len();
        self.tail.extend_from_slice(&pcm[..missing.min(pcm.len())]);
        // The last `search` frames discarded so far.
        let max = self.search * ch;
        if pcm.len() >= max {
            self.last.clear();
            self.last.extend_from_slice(&pcm[pcm.len() - max..]);
        } else {
            let excess = (self.last.len() + pcm.len()).saturating_sub(max);
            self.last.drain(..excess);
            self.last.extend_from_slice(pcm);
        }
    }

    /// The first frame played after discarded audio: returns `(head, rest)`, the spliced
    /// audio to play instead of `pcm` (`head` followed by `rest`, a suffix of `pcm`). Without
    /// discarded audio that is `([], pcm)`.
    pub fn splice<'a>(&'a mut self, pcm: &'a [f32]) -> (&'a [f32], &'a [f32]) {
        let ch = self.channels;
        let pcm = &pcm[..pcm.len() / ch * ch];
        if !std::mem::take(&mut self.pending) {
            return (&[], pcm);
        }
        // Candidate audio after the cut: the end of the discarded audio, then this frame.
        let last_frames = self.last.len() / ch;
        let post_frames = last_frames + pcm.len() / ch;
        let fade = (self.tail.len() / ch).min(post_frames);
        if fade == 0 {
            return (&[], pcm);
        }
        self.splices += 1;
        let post = |i: usize, c: usize| -> f32 {
            if i < last_frames {
                self.last[i * ch + c]
            } else {
                pcm[(i - last_frames) * ch + c]
            }
        };
        // Splice points k: the crossfade takes post[k..k + fade]. k = last_frames is the
        // nominal cut (exactly the discarded frames).
        let nominal = last_frames;
        let k_max = (last_frames + self.search).min(post_frames - fade);
        self.tail_sum.clear();
        self.tail_sum.extend(
            self.tail
                .chunks_exact(ch)
                .take(fade)
                .map(|f| f.iter().map(|v| f64::from(*v)).sum::<f64>()),
        );
        self.post_sum.clear();
        self.post_sum
            .extend((0..k_max + fade).map(|i| (0..ch).map(|c| f64::from(post(i, c))).sum::<f64>()));
        let (k, corr) = best_match(
            &self.tail_sum,
            &self.post_sum,
            nominal.min(k_max),
            self.search,
        );
        // Crossfade (correlation-compensated equal power), then the rest of `last`, if the
        // splice point lies inside it, into `head`; the rest of `pcm` is played as it is.
        let corr = corr.clamp(0.0, 1.0);
        self.head.clear();
        for i in 0..fade {
            let theta = std::f64::consts::FRAC_PI_2 * (i as f64 + 0.5) / fade as f64;
            let norm = (1.0 + corr * (2.0 * theta).sin()).sqrt();
            let (a, b) = ((theta.cos() / norm) as f32, (theta.sin() / norm) as f32);
            for c in 0..ch {
                self.head
                    .push(a * self.tail[i * ch + c] + b * post(k + i, c));
            }
        }
        let resume = k + fade;
        if resume < last_frames {
            self.head.extend_from_slice(&self.last[resume * ch..]);
        }
        let rest = &pcm[resume.saturating_sub(last_frames) * ch..];
        (&self.head, rest)
    }
}

/// The splice point `k` (and its normalized correlation) where `post[k..k + tail.len()]`
/// best matches `tail`, for `k` in `0..=post.len() - tail.len()`, penalizing the distance
/// from `nominal` (at most [`OFFSET_PENALTY`] at `span` frames away).
fn best_match(tail: &[f64], post: &[f64], nominal: usize, span: usize) -> (usize, f64) {
    let n = tail.len();
    let tail_energy: f64 = tail.iter().map(|v| v * v).sum();
    // Below this energy a signal counts as silence (−150 dBFS per sample): no correlation.
    let floor = 1e-15 * n as f64;
    let mut post_energy: f64 = post[..n].iter().map(|v| v * v).sum();
    let mut best = (nominal, 0.0, f64::NEG_INFINITY);
    for k in 0..=post.len() - n {
        if k > 0 {
            post_energy += post[k + n - 1] * post[k + n - 1] - post[k - 1] * post[k - 1];
        }
        let corr = if tail_energy > floor && post_energy > floor {
            let dot: f64 = tail.iter().zip(&post[k..k + n]).map(|(a, b)| a * b).sum();
            dot / (tail_energy * post_energy.max(floor)).sqrt()
        } else {
            0.0
        };
        let score = corr - OFFSET_PENALTY * k.abs_diff(nominal) as f64 / span.max(1) as f64;
        if score > best.2 {
            best = (k, corr, score);
        }
    }
    (best.0, best.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    const FRAME: usize = 480;

    /// `frames` of a stereo sine (the same on both channels), from sample `start`.
    fn sine(freq: f64, amp: f32, start: usize, frames: usize) -> Vec<f32> {
        (start..start + frames)
            .flat_map(|n| {
                let v =
                    amp * (std::f64::consts::TAU * freq * n as f64 / f64::from(RATE)).sin() as f32;
                [v, v]
            })
            .collect()
    }

    /// Plays `frames` 10 ms frames of `signal(start, frames)`, discarding frames `cut` (a
    /// range of frame indices) through a splicer; returns the left channel.
    fn play(
        signal: &dyn Fn(usize, usize) -> Vec<f32>,
        frames: usize,
        cut: std::ops::Range<usize>,
    ) -> Vec<f32> {
        let mut sp = Splicer::new(RATE, 2);
        let mut out = Vec::new();
        for f in 0..frames {
            let pcm = signal(f * FRAME, FRAME);
            if cut.contains(&f) {
                sp.discard(&pcm);
                continue;
            }
            let (head, rest) = sp.splice(&pcm);
            out.extend(head.iter().chain(rest).step_by(2));
        }
        out
    }

    /// Largest sample-to-sample step of `x` (a click shows up as a large step).
    fn max_step(x: &[f32]) -> f32 {
        x.windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0, f32::max)
    }

    #[test]
    fn a_cut_of_a_steady_tone_continues_in_phase() {
        for freq in [440.0, 1000.0, 97.0, 3_300.0] {
            let signal = |start, frames| sine(freq, 0.25, start, frames);
            let out = play(&signal, 30, 10..11);
            // Ideally the next sample of a sine continues it: the step never exceeds the
            // sine's own largest step (plus a little for the crossfade).
            let own = (std::f64::consts::TAU * freq / f64::from(RATE)).sin() as f32 * 0.25;
            let step = max_step(&out);
            assert!(step <= own * 1.1 + 1e-4, "{freq} Hz: step {step} vs {own}");
            // About one frame shorter, within the search span.
            let cut = 30 * FRAME - out.len();
            assert!(cut.abs_diff(FRAME) <= 240, "{freq} Hz: cut {cut}");
            // The level never dips or bumps: the envelope over each half period stays put.
            let period = (f64::from(RATE) / freq).ceil() as usize;
            for w in out.chunks(period).filter(|w| w.len() == period) {
                let peak = w.iter().fold(0.0f32, |m, v| m.max(v.abs()));
                assert!((0.23..=0.26).contains(&peak), "{freq} Hz: peak {peak}");
            }
        }
    }

    #[test]
    fn a_plain_cut_would_click() {
        // The control: what the splicer avoids (10 ms of 440 Hz is 4.4 periods: a 144° jump).
        let mut out = Vec::new();
        for f in (0..30).filter(|f| *f != 10) {
            out.extend(sine(440.0, 0.25, f * FRAME, FRAME).iter().step_by(2));
        }
        let own = (std::f64::consts::TAU * 440.0 / f64::from(RATE)).sin() as f32 * 0.25;
        assert!(max_step(&out) > 5.0 * own);
    }

    #[test]
    fn long_cuts_and_unrelated_audio_are_crossfaded_without_a_step() {
        // A 70 ms cut of a two-tone mix, then a cut from a tone into noise.
        let two = |start, frames| {
            let a = sine(440.0, 0.2, start, frames);
            let b = sine(1234.5, 0.1, start, frames);
            a.iter().zip(&b).map(|(x, y)| x + y).collect::<Vec<f32>>()
        };
        let out = play(&two, 40, 10..17);
        assert!(out.len().abs_diff((40 - 7) * FRAME) <= 240, "{}", out.len());
        assert!(max_step(&out) < 0.05, "{}", max_step(&out));

        let mut sp = Splicer::new(RATE, 2);
        let mut seed = 1u32;
        let mut noise = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
        };
        sp.discard(&sine(440.0, 0.25, 0, FRAME));
        let frame: Vec<f32> = (0..FRAME)
            .flat_map(|_| {
                let v = noise() * 0.1;
                [v, v]
            })
            .collect();
        let fade = sp.crossfade_frames();
        let (head, rest) = sp.splice(&frame);
        assert_eq!(head.len() / 2, fade);
        // Starts where the tone left off (sin 0 = 0), ends in the noise.
        assert!(head[0].abs() < 0.01, "{}", head[0]);
        assert!(rest.len() + head.len() <= 2 * FRAME + 2 * 240);
        // Equal power for unrelated audio: no gain above either input.
        assert!(head.iter().all(|v| v.abs() <= 0.25 * 1.01 + 0.05));
    }

    #[test]
    fn without_a_cut_frames_pass_through_and_reset_forgets() {
        let mut sp = Splicer::new(RATE, 2);
        let pcm = sine(440.0, 0.25, 0, FRAME);
        let (head, rest) = sp.splice(&pcm);
        assert!(head.is_empty());
        assert_eq!(rest, &pcm[..]);
        sp.discard(&pcm);
        assert!(sp.is_pending());
        sp.reset();
        assert!(!sp.is_pending());
        let (head, rest) = sp.splice(&pcm);
        assert!(head.is_empty() && rest.len() == pcm.len());
        assert_eq!(sp.splices(), 0);
        // Silence is spliced at the nominal point.
        let silence = vec![0.0f32; 2 * FRAME];
        sp.discard(&silence);
        let (head, rest) = sp.splice(&silence);
        assert_eq!(head.len() + rest.len(), silence.len());
        assert_eq!(sp.splices(), 1);
    }
}
