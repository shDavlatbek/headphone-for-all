//! Sample-format and channel conversion. All functions are allocation-free when `out` already
//! has enough capacity, so they are usable on soft real-time threads.

/// Converts `i16` PCM to `f32` in [-1, 1] (`x / 32768`). Converts
/// `min(input.len(), out.len())` samples.
pub fn i16_to_f32(_input: &[i16], _out: &mut [f32]) {
    todo!("feat/audio")
}

/// Converts `f32` to `i16` PCM, clamping to [-1, 1] first. Converts
/// `min(input.len(), out.len())` samples.
pub fn f32_to_i16(_input: &[f32], _out: &mut [i16]) {
    todo!("feat/audio")
}

/// Converts interleaved audio with `in_channels` channels to interleaved stereo.
///
/// `out` is cleared and then receives `frames * 2` samples. Mono is duplicated to both
/// channels; stereo is copied; more than 2 channels are folded down (front L/R plus the
/// remaining channels mixed equally into both sides, scaled to avoid clipping).
/// A trailing partial frame in `input` is ignored.
pub fn to_stereo(_input: &[f32], _in_channels: u16, _out: &mut Vec<f32>) {
    todo!("feat/audio")
}
