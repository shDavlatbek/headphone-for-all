//! Sample-format and channel conversion. All functions are allocation-free when `out` already
//! has enough capacity, so they are usable on soft real-time threads.

/// Weight of each channel beyond front L/R when folding down to stereo (−3 dB).
const FOLD_WEIGHT: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Converts `i16` PCM to `f32` in [-1, 1] (`x / 32768`). Converts
/// `min(input.len(), out.len())` samples.
pub fn i16_to_f32(input: &[i16], out: &mut [f32]) {
    for (o, &i) in out.iter_mut().zip(input) {
        *o = f32::from(i) / 32_768.0;
    }
}

/// Converts `f32` to `i16` PCM, clamping to [-1, 1] first. Converts
/// `min(input.len(), out.len())` samples.
///
/// The scale is `x · 32768` rounded to nearest and saturated to `i16`, the exact inverse of
/// [`i16_to_f32`] (every `i16` round-trips). NaN becomes 0.
pub fn f32_to_i16(input: &[f32], out: &mut [i16]) {
    for (o, &x) in out.iter_mut().zip(input) {
        let x = if x.is_nan() { 0.0 } else { x.clamp(-1.0, 1.0) };
        // In range after the clamp: [-32768.0, 32768.0] saturated to i16 by `as`.
        *o = (x * 32_768.0).round().clamp(-32_768.0, 32_767.0) as i16;
    }
}

/// Converts interleaved audio with `in_channels` channels to interleaved stereo.
///
/// `out` is cleared and then receives `frames * 2` samples. Mono is duplicated to both
/// channels; stereo is copied; more than 2 channels are folded down (front L/R plus the
/// remaining channels mixed equally into both sides, scaled to avoid clipping).
/// A trailing partial frame in `input` is ignored.
///
/// Fold-down for `n > 2` channels: `L = (c0 + w·Σc[2..n]) / (1 + w·(n−2))` and
/// `R = (c1 + w·Σc[2..n]) / (1 + w·(n−2))` with `w = 1/√2`, so full-scale input can never
/// exceed full scale. `in_channels == 0` produces no output.
pub fn to_stereo(input: &[f32], in_channels: u16, out: &mut Vec<f32>) {
    out.clear();
    let ch = usize::from(in_channels);
    if ch == 0 {
        return;
    }
    let frames = input.len() / ch;
    out.reserve(frames * 2);
    match ch {
        1 => {
            for &x in &input[..frames] {
                out.push(x);
                out.push(x);
            }
        }
        2 => out.extend_from_slice(&input[..frames * 2]),
        _ => {
            let norm = 1.0 / (1.0 + FOLD_WEIGHT * (ch - 2) as f32);
            for frame in input.chunks_exact(ch) {
                let rest: f32 = frame[2..].iter().sum::<f32>() * FOLD_WEIGHT;
                out.push((frame[0] + rest) * norm);
                out.push((frame[1] + rest) * norm);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn i16_f32_roundtrip_is_exact() {
        let all: Vec<i16> = (i16::MIN..=i16::MAX).collect();
        let mut f = vec![0.0_f32; all.len()];
        i16_to_f32(&all, &mut f);
        assert_eq!(f[0], -1.0);
        assert!(f.iter().all(|x| (-1.0..1.0).contains(x)));
        let mut back = vec![0_i16; all.len()];
        f32_to_i16(&f, &mut back);
        assert_eq!(back, all);
    }

    #[test]
    fn f32_to_i16_clamps_and_handles_nan() {
        let mut out = [0_i16; 6];
        f32_to_i16(&[2.0, -2.0, 1.0, -1.0, f32::NAN, 0.5], &mut out);
        assert_eq!(out, [32_767, -32_768, 32_767, -32_768, 0, 16_384]);
    }

    #[test]
    fn conversion_uses_shortest_length() {
        let mut out = [7.0_f32; 4];
        i16_to_f32(&[16_384, -16_384], &mut out);
        assert_eq!(out, [0.5, -0.5, 7.0, 7.0]);
        let mut out = [0_i16; 1];
        f32_to_i16(&[0.25, 0.5], &mut out);
        assert_eq!(out, [8_192]);
    }

    #[test]
    fn to_stereo_mono_stereo_partial() {
        let mut out = vec![1.0; 3];
        to_stereo(&[0.1, 0.2, 0.3], 1, &mut out);
        assert_eq!(out, vec![0.1, 0.1, 0.2, 0.2, 0.3, 0.3]);
        to_stereo(&[0.1, 0.2, 0.3, 0.4, 0.5], 2, &mut out);
        assert_eq!(out, vec![0.1, 0.2, 0.3, 0.4]);
        to_stereo(&[0.1, 0.2], 0, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn to_stereo_folds_down_without_clipping() {
        // 5.1 at full scale on every channel must stay within [-1, 1].
        let mut out = Vec::new();
        to_stereo(&[1.0; 6 * 4], 6, &mut out);
        assert_eq!(out.len(), 8);
        assert!(out.iter().all(|x| (*x - 1.0).abs() < 1e-6), "{out:?}");
        // Only front left: appears on L only, attenuated by the normalization.
        to_stereo(&[1.0, 0.0, 0.0, 0.0], 4, &mut out);
        let norm = 1.0 / (1.0 + 2.0 * FOLD_WEIGHT);
        assert!((out[0] - norm).abs() < 1e-6);
        assert_eq!(out[1], 0.0);
        // A surround channel goes equally to both sides.
        to_stereo(&[0.0, 0.0, 1.0], 3, &mut out);
        assert!((out[0] - out[1]).abs() < 1e-7 && out[0] > 0.0);
    }
}
