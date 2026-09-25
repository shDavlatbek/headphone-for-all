//! Opus codec wrapper over `opusic-sys` (libopus built statically from bundled source).
//!
//! PCM is interleaved `f32`. "Frame size" follows libopus terminology: samples **per channel**.
//!
//! Encoder settings: `OPUS_APPLICATION_AUDIO` (or `RESTRICTED_LOWDELAY` when
//! [`OpusConfig::low_delay`]), VBR, complexity [`ENCODER_COMPLEXITY`], in-band FEC mode 1 plus
//! `OPUS_SET_PACKET_LOSS_PERC` when [`OpusConfig::fec`]. Note that libopus only carries in-band
//! FEC in SILK/hybrid packets: at music bitrates the encoder stays in CELT mode until the
//! expected loss is high enough (≈ 8 % for music), and for a CELT packet
//! [`OpusDecoder::decode_fec`] falls back to packet-loss concealment. Both paths always return
//! the requested number of frames.

use std::ffi::{c_int, CStr};
use std::ptr::NonNull;

use opusic_sys as ffi;
use serde::{Deserialize, Serialize};

use crate::error::AudioError;
use crate::Result;

/// Maximum size of one Opus packet in bytes (RFC 6716).
pub const MAX_OPUS_PACKET: usize = 1275;

/// Encoder complexity (0..=10). Lower on mobile to save battery.
#[cfg(any(target_os = "android", target_os = "ios"))]
pub const ENCODER_COMPLEXITY: i32 = 7;
/// Encoder complexity (0..=10). Lower on mobile to save battery.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub const ENCODER_COMPLEXITY: i32 = 10;

/// Sample rates libopus accepts.
const VALID_RATES: [u32; 5] = [8_000, 12_000, 16_000, 24_000, 48_000];
/// Frame durations (whole ms) libopus accepts. 2.5 ms is also valid for libopus but cannot be
/// expressed in the integer `frame_ms` field.
const VALID_FRAME_MS: [u32; 5] = [5, 10, 20, 40, 60];

/// Converts a libopus error code into an [`AudioError::Opus`].
fn opus_error(code: c_int) -> AudioError {
    // SAFETY: `opus_strerror` accepts any integer and returns a pointer to a static,
    // NUL-terminated string (never NULL).
    let message = unsafe {
        let p = ffi::opus_strerror(code);
        if p.is_null() {
            String::from("unknown error")
        } else {
            CStr::from_ptr(p).to_string_lossy().into_owned()
        }
    };
    AudioError::Opus { code, message }
}

/// Maps a libopus return value (negative = error) to a `Result`.
fn check(ret: c_int) -> Result<c_int> {
    if ret < 0 {
        Err(opus_error(ret))
    } else {
        Ok(ret)
    }
}

fn validate_rate_channels(sample_rate: u32, channels: u16) -> Result<()> {
    if !VALID_RATES.contains(&sample_rate) {
        return Err(AudioError::InvalidConfig(format!(
            "opus sample rate {sample_rate} Hz (expected one of {VALID_RATES:?})"
        )));
    }
    if !(1..=2).contains(&channels) {
        return Err(AudioError::InvalidConfig(format!(
            "opus channel count {channels} (expected 1 or 2)"
        )));
    }
    Ok(())
}

fn validate_bitrate(bitrate: u32) -> Result<()> {
    if !(6_000..=510_000).contains(&bitrate) {
        return Err(AudioError::InvalidConfig(format!(
            "opus bitrate {bitrate} bit/s (expected 6000..=510000)"
        )));
    }
    Ok(())
}

fn validate_loss(pct: u8) -> Result<()> {
    if pct > 100 {
        return Err(AudioError::InvalidConfig(format!(
            "expected packet loss {pct} % (expected 0..=100)"
        )));
    }
    Ok(())
}

/// Frames per channel in `out`, validated to be a whole, non-empty multiple of 2.5 ms (what
/// libopus requires for FEC and PLC).
fn plc_frames(out_len: usize, channels: u16, sample_rate: u32) -> Result<usize> {
    let ch = usize::from(channels);
    let quantum = (sample_rate / 400) as usize; // 2.5 ms
    if out_len == 0 || !out_len.is_multiple_of(ch) || !(out_len / ch).is_multiple_of(quantum) {
        let frames = out_len / ch;
        let expected = frames.max(quantum).div_ceil(quantum) * quantum * ch;
        return Err(AudioError::BufferSize {
            expected,
            got: out_len,
        });
    }
    Ok(out_len / ch)
}

/// Clamps a buffer length to what libopus accepts as `c_int`.
fn len_c_int(len: usize) -> c_int {
    c_int::try_from(len).unwrap_or(c_int::MAX)
}

/// Encoder configuration.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct OpusConfig {
    /// 8 000, 12 000, 16 000, 24 000 or 48 000 Hz.
    pub sample_rate: u32,
    /// 1 or 2.
    pub channels: u16,
    /// Target bitrate in bits per second (6 000..=510 000).
    pub bitrate: u32,
    /// Frame duration in ms: 10 or 20 (5, 40, 60 are also accepted by libopus).
    pub frame_ms: u32,
    /// Enable in-band forward error correction.
    pub fec: bool,
    /// Expected packet loss in percent (0..=100); tunes FEC.
    pub expected_loss_pct: u8,
    /// Use `OPUS_APPLICATION_RESTRICTED_LOWDELAY` instead of `OPUS_APPLICATION_AUDIO`.
    pub low_delay: bool,
}

impl Default for OpusConfig {
    /// 48 kHz stereo, 128 kbit/s, 10 ms, FEC on with 5 % expected loss, `APPLICATION_AUDIO`.
    fn default() -> Self {
        Self {
            sample_rate: 48_000,
            channels: 2,
            bitrate: 128_000,
            frame_ms: 10,
            fec: true,
            expected_loss_pct: 5,
            low_delay: false,
        }
    }
}

/// Opus encoder. `Send` (moved into the sender's encoder thread), not `Sync`.
pub struct OpusEncoder {
    config: OpusConfig,
    frame_samples: usize,
    st: NonNull<ffi::OpusEncoder>,
}

// SAFETY: the libopus encoder state is a plain heap block without thread affinity; it is only
// accessed through `&mut self` (or `&self` for nothing that touches it), so moving it to
// another thread is sound. It is deliberately not `Sync`.
unsafe impl Send for OpusEncoder {}

impl std::fmt::Debug for OpusEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpusEncoder")
            .field("config", &self.config)
            .field("frame_samples", &self.frame_samples)
            .finish_non_exhaustive()
    }
}

impl Drop for OpusEncoder {
    fn drop(&mut self) {
        // SAFETY: `st` came from `opus_encoder_create` and is destroyed exactly once.
        unsafe { ffi::opus_encoder_destroy(self.st.as_ptr()) }
    }
}

impl OpusEncoder {
    /// Creates an encoder.
    ///
    /// # Errors
    /// [`crate::AudioError::InvalidConfig`] or [`crate::AudioError::Opus`].
    pub fn new(config: OpusConfig) -> Result<Self> {
        validate_rate_channels(config.sample_rate, config.channels)?;
        validate_bitrate(config.bitrate)?;
        validate_loss(config.expected_loss_pct)?;
        if !VALID_FRAME_MS.contains(&config.frame_ms) {
            return Err(AudioError::InvalidConfig(format!(
                "opus frame duration {} ms (expected one of {VALID_FRAME_MS:?})",
                config.frame_ms
            )));
        }
        let application = if config.low_delay {
            ffi::OPUS_APPLICATION_RESTRICTED_LOWDELAY
        } else {
            ffi::OPUS_APPLICATION_AUDIO
        };
        let mut err: c_int = 0;
        // SAFETY: arguments were validated above; `err` is a valid out pointer.
        let raw = unsafe {
            ffi::opus_encoder_create(
                config.sample_rate as ffi::opus_int32,
                c_int::from(config.channels),
                application,
                &mut err,
            )
        };
        check(err)?;
        let st = NonNull::new(raw).ok_or_else(|| opus_error(ffi::OPUS_ALLOC_FAIL))?;
        let frame_samples = (config.sample_rate as usize) * (config.frame_ms as usize) / 1000;
        let mut enc = Self {
            config,
            frame_samples,
            st,
        };
        enc.ctl(ffi::OPUS_SET_BITRATE_REQUEST, config.bitrate as c_int)?;
        enc.ctl(ffi::OPUS_SET_VBR_REQUEST, 1)?;
        enc.ctl(ffi::OPUS_SET_COMPLEXITY_REQUEST, ENCODER_COMPLEXITY)?;
        enc.apply_fec(config.fec, config.expected_loss_pct)?;
        Ok(enc)
    }

    /// Calls `opus_encoder_ctl` with one integer argument.
    fn ctl(&mut self, request: c_int, value: c_int) -> Result<()> {
        // SAFETY: `st` is a live encoder; every request used here takes one `opus_int32`.
        check(unsafe { ffi::opus_encoder_ctl(self.st.as_ptr(), request, value) }).map(|_| ())
    }

    fn apply_fec(&mut self, fec: bool, loss_pct: u8) -> Result<()> {
        self.ctl(ffi::OPUS_SET_INBAND_FEC_REQUEST, c_int::from(fec))?;
        let loss = if fec { c_int::from(loss_pct) } else { 0 };
        self.ctl(ffi::OPUS_SET_PACKET_LOSS_PERC_REQUEST, loss)
    }

    /// Encoder algorithmic delay (lookahead) in samples per channel at the configured rate.
    /// Decoded audio lags the input by this amount.
    ///
    /// # Errors
    /// [`crate::AudioError::Opus`].
    pub fn lookahead(&mut self) -> Result<usize> {
        let mut value: ffi::opus_int32 = 0;
        // SAFETY: OPUS_GET_LOOKAHEAD takes a valid `opus_int32*` out pointer.
        check(unsafe {
            ffi::opus_encoder_ctl(
                self.st.as_ptr(),
                ffi::OPUS_GET_LOOKAHEAD_REQUEST,
                &mut value as *mut ffi::opus_int32,
            )
        })?;
        Ok(usize::try_from(value).unwrap_or(0))
    }

    /// The configuration the encoder currently uses (reflects `set_*` calls).
    pub fn config(&self) -> &OpusConfig {
        &self.config
    }

    /// Encodes exactly one frame (`frame_samples() * channels` interleaved samples) into `out`
    /// and returns the packet length in bytes. `out` should be at least [`MAX_OPUS_PACKET`].
    ///
    /// # Errors
    /// [`crate::AudioError::BufferSize`] or [`crate::AudioError::Opus`].
    pub fn encode(&mut self, pcm: &[f32], out: &mut [u8]) -> Result<usize> {
        let expected = self.frame_samples * usize::from(self.config.channels);
        if pcm.len() != expected {
            return Err(AudioError::BufferSize {
                expected,
                got: pcm.len(),
            });
        }
        if out.is_empty() {
            return Err(AudioError::BufferSize {
                expected: MAX_OPUS_PACKET,
                got: 0,
            });
        }
        // SAFETY: `pcm` holds exactly `frame_samples * channels` samples; `out` is writable
        // for `out.len()` bytes (clamped to c_int).
        let n = check(unsafe {
            ffi::opus_encode_float(
                self.st.as_ptr(),
                pcm.as_ptr(),
                self.frame_samples as c_int,
                out.as_mut_ptr(),
                len_c_int(out.len()),
            )
        })?;
        Ok(n as usize)
    }

    /// Changes the target bitrate (bits per second).
    ///
    /// # Errors
    /// [`crate::AudioError::Opus`] if libopus rejects the value.
    pub fn set_bitrate(&mut self, bitrate: u32) -> Result<()> {
        validate_bitrate(bitrate).map_err(|_| opus_error(ffi::OPUS_BAD_ARG))?;
        self.ctl(ffi::OPUS_SET_BITRATE_REQUEST, bitrate as c_int)?;
        self.config.bitrate = bitrate;
        Ok(())
    }

    /// Changes the expected packet loss percentage (0..=100).
    ///
    /// # Errors
    /// [`crate::AudioError::Opus`] if libopus rejects the value.
    pub fn set_expected_loss(&mut self, pct: u8) -> Result<()> {
        validate_loss(pct).map_err(|_| opus_error(ffi::OPUS_BAD_ARG))?;
        self.apply_fec(self.config.fec, pct)?;
        self.config.expected_loss_pct = pct;
        Ok(())
    }

    /// Enables or disables in-band FEC.
    ///
    /// # Errors
    /// [`crate::AudioError::Opus`] if libopus rejects the value.
    pub fn set_fec(&mut self, enabled: bool) -> Result<()> {
        self.apply_fec(enabled, self.config.expected_loss_pct)?;
        self.config.fec = enabled;
        Ok(())
    }

    /// Frame size in samples **per channel** (e.g. 480 for 10 ms at 48 kHz).
    pub fn frame_samples(&self) -> usize {
        self.frame_samples
    }
}

/// Opus decoder with FEC recovery and packet-loss concealment. `Send`, not `Sync`.
pub struct OpusDecoder {
    sample_rate: u32,
    channels: u16,
    st: NonNull<ffi::OpusDecoder>,
}

// SAFETY: see `OpusEncoder`; the decoder state has no thread affinity and is only touched
// through `&mut self`.
unsafe impl Send for OpusDecoder {}

impl std::fmt::Debug for OpusDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpusDecoder")
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .finish_non_exhaustive()
    }
}

impl Drop for OpusDecoder {
    fn drop(&mut self) {
        // SAFETY: `st` came from `opus_decoder_create` and is destroyed exactly once.
        unsafe { ffi::opus_decoder_destroy(self.st.as_ptr()) }
    }
}

impl OpusDecoder {
    /// Creates a decoder producing `sample_rate`/`channels` interleaved `f32`.
    ///
    /// # Errors
    /// [`crate::AudioError::InvalidConfig`] or [`crate::AudioError::Opus`].
    pub fn new(sample_rate: u32, channels: u16) -> Result<Self> {
        validate_rate_channels(sample_rate, channels)?;
        let mut err: c_int = 0;
        // SAFETY: arguments were validated; `err` is a valid out pointer.
        let raw = unsafe {
            ffi::opus_decoder_create(
                sample_rate as ffi::opus_int32,
                c_int::from(channels),
                &mut err,
            )
        };
        check(err)?;
        let st = NonNull::new(raw).ok_or_else(|| opus_error(ffi::OPUS_ALLOC_FAIL))?;
        Ok(Self {
            sample_rate,
            channels,
            st,
        })
    }

    /// Runs `opus_decode_float`. `data = None` means PLC.
    fn decode_raw(&mut self, data: Option<&[u8]>, out: &mut [f32], fec: bool) -> Result<usize> {
        let frame_size = len_c_int(out.len() / usize::from(self.channels));
        let (ptr, len) = match data {
            Some(d) => (d.as_ptr(), len_c_int(d.len())),
            None => (std::ptr::null(), 0),
        };
        // SAFETY: `ptr`/`len` describe a readable buffer (or NULL/0 for PLC); `out` is
        // writable for `frame_size * channels` samples.
        let n = check(unsafe {
            ffi::opus_decode_float(
                self.st.as_ptr(),
                ptr,
                len,
                out.as_mut_ptr(),
                frame_size,
                c_int::from(fec),
            )
        })?;
        Ok(n as usize)
    }

    /// Output sample rate.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Output channel count.
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Decodes one packet into `out` (interleaved) and returns the number of frames
    /// (samples per channel) written. `out` must hold at least one full frame.
    ///
    /// # Errors
    /// [`crate::AudioError::Opus`] on a corrupt packet.
    ///
    /// An empty `packet` is rejected with `OPUS_BAD_ARG` (use [`OpusDecoder::conceal`] for a
    /// lost frame).
    pub fn decode(&mut self, packet: &[u8], out: &mut [f32]) -> Result<usize> {
        if packet.is_empty() {
            return Err(opus_error(ffi::OPUS_BAD_ARG));
        }
        self.decode_raw(Some(packet), out, false)
    }

    /// Recovers the *previous*, lost frame from the FEC data in `next_packet`. The number of
    /// frames recovered equals `out.len() / channels`. Returns frames written.
    ///
    /// # Errors
    /// [`crate::AudioError::Opus`].
    ///
    /// `out.len() / channels` must be a non-zero multiple of 2.5 ms (normally exactly one
    /// frame), else [`crate::AudioError::BufferSize`]. If `next_packet` carries no FEC data
    /// (e.g. CELT-only packets), libopus conceals the frame instead. After this call, decode
    /// `next_packet` normally with [`OpusDecoder::decode`].
    pub fn decode_fec(&mut self, next_packet: &[u8], out: &mut [f32]) -> Result<usize> {
        plc_frames(out.len(), self.channels, self.sample_rate)?;
        if next_packet.is_empty() {
            return self.conceal(out);
        }
        self.decode_raw(Some(next_packet), out, true)
    }

    /// Packet-loss concealment: synthesizes `out.len() / channels` frames. Returns frames
    /// written.
    ///
    /// # Errors
    /// [`crate::AudioError::Opus`].
    ///
    /// `out.len() / channels` must be a non-zero multiple of 2.5 ms, else
    /// [`crate::AudioError::BufferSize`].
    pub fn conceal(&mut self, out: &mut [f32]) -> Result<usize> {
        plc_frames(out.len(), self.channels, self.sample_rate)?;
        self.decode_raw(None, out, false)
    }

    /// Resets decoder state (after a stream reset).
    pub fn reset(&mut self) {
        // SAFETY: OPUS_RESET_STATE takes no argument; `st` is a live decoder. It cannot fail
        // for a valid decoder, so the return value is ignored.
        unsafe {
            ffi::opus_decoder_ctl(self.st.as_ptr(), ffi::OPUS_RESET_STATE);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::AudioFormat;
    use crate::tone::SineGenerator;

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>() / x.len() as f64).sqrt()
    }

    /// Frequency of channel 0 of an interleaved buffer from the rising zero crossings
    /// (linearly interpolated), in Hz.
    fn zero_crossing_freq(x: &[f32], channels: usize, rate: f64) -> f64 {
        let mono: Vec<f32> = x.chunks_exact(channels).map(|f| f[0]).collect();
        let mut crossings = Vec::new();
        for i in 1..mono.len() {
            if mono[i - 1] < 0.0 && mono[i] >= 0.0 {
                let t = f64::from(mono[i - 1]) / f64::from(mono[i - 1] - mono[i]);
                crossings.push((i - 1) as f64 + t);
            }
        }
        let n = crossings.len();
        assert!(n > 2, "no signal");
        (n - 1) as f64 * rate / (crossings[n - 1] - crossings[0])
    }

    /// Encodes `seconds` of a 1 kHz sine and decodes every packet.
    fn roundtrip(cfg: OpusConfig, seconds: usize) -> (Vec<f32>, Vec<f32>, usize) {
        let mut enc = OpusEncoder::new(cfg).unwrap();
        let mut dec = OpusDecoder::new(cfg.sample_rate, cfg.channels).unwrap();
        let fmt = AudioFormat::new(cfg.sample_rate, cfg.channels);
        let mut gen = SineGenerator::new(1000.0, 0.5, fmt);
        let frame = enc.frame_samples() * usize::from(cfg.channels);
        let mut pcm = vec![0.0; frame];
        let mut pkt = [0_u8; MAX_OPUS_PACKET];
        let mut dec_buf = vec![0.0; frame];
        let (mut input, mut output) = (Vec::new(), Vec::new());
        let frames = seconds * 1000 / cfg.frame_ms as usize;
        for _ in 0..frames {
            gen.fill(&mut pcm);
            input.extend_from_slice(&pcm);
            let n = enc.encode(&pcm, &mut pkt).unwrap();
            assert!(n > 0 && n <= MAX_OPUS_PACKET);
            let got = dec.decode(&pkt[..n], &mut dec_buf).unwrap();
            assert_eq!(got, enc.frame_samples());
            output.extend_from_slice(&dec_buf);
        }
        let delay = enc.lookahead().unwrap();
        (input, output, delay)
    }

    #[test]
    fn sine_roundtrip_128k_keeps_frequency_and_level() {
        let cfg = OpusConfig::default();
        let (input, output, delay) = roundtrip(cfg, 2);
        assert!(delay > 0 && delay < 960, "lookahead {delay}");
        // Skip the codec delay plus 100 ms of encoder warm-up.
        let skip = (delay + 4_800) * 2;
        let out = &output[skip..];
        let inp = &input[skip..];
        let f = zero_crossing_freq(out, 2, 48_000.0);
        assert!((f - 1000.0).abs() < 1.0, "decoded frequency {f}");
        let db = 20.0 * (rms(out) / rms(inp)).log10();
        assert!(db.abs() < 1.0, "level difference {db} dB");
        // The decoded waveform lines up with the input once shifted by the lookahead (CELT is
        // perceptual, so the sample-exact SNR of a pure tone is modest, ~17 dB; a wrong
        // alignment by a few samples drops it well below 10 dB).
        let snr_at = |d: usize| {
            let diff: Vec<f32> = input
                .iter()
                .zip(&output[d * 2..])
                .map(|(a, b)| a - b)
                .collect();
            20.0 * (rms(inp) / rms(&diff[skip..])).log10()
        };
        let snr = snr_at(delay);
        assert!(snr > 12.0, "snr {snr} dB at the lookahead");
        assert!(snr > snr_at(delay + 4) && snr > snr_at(delay - 4));
    }

    #[test]
    fn low_delay_mono_20ms_roundtrip() {
        let cfg = OpusConfig {
            channels: 1,
            frame_ms: 20,
            low_delay: true,
            fec: false,
            bitrate: 64_000,
            ..OpusConfig::default()
        };
        let (_, output, delay) = roundtrip(cfg, 1);
        let f = zero_crossing_freq(&output[delay + 4_800..], 1, 48_000.0);
        assert!((f - 1000.0).abs() < 1.0, "decoded frequency {f}");
    }

    #[test]
    fn fec_and_plc_return_frame_counts() {
        // Low bitrate + high expected loss makes libopus use SILK/hybrid with real LBRR data.
        for cfg in [
            OpusConfig {
                bitrate: 32_000,
                expected_loss_pct: 30,
                frame_ms: 20,
                ..OpusConfig::default()
            },
            OpusConfig::default(),
        ] {
            let mut enc = OpusEncoder::new(cfg).unwrap();
            let mut dec = OpusDecoder::new(48_000, 2).unwrap();
            let fs = enc.frame_samples();
            let mut gen = SineGenerator::new(440.0, 0.3, AudioFormat::INTERNAL);
            let mut pcm = vec![0.0; fs * 2];
            let mut out = vec![0.0; fs * 2];
            let mut pkts = Vec::new();
            for _ in 0..50 {
                gen.fill(&mut pcm);
                let mut p = vec![0_u8; MAX_OPUS_PACKET];
                let n = enc.encode(&pcm, &mut p).unwrap();
                p.truncate(n);
                pkts.push(p);
            }
            for (i, p) in pkts.iter().enumerate() {
                if i % 7 == 3 {
                    // Lost: recover it from the next packet's FEC.
                    assert_eq!(dec.decode_fec(&pkts[i + 1], &mut out).unwrap(), fs);
                } else if i % 11 == 5 {
                    assert_eq!(dec.conceal(&mut out).unwrap(), fs);
                } else {
                    assert_eq!(dec.decode(p, &mut out).unwrap(), fs);
                }
                assert!(out.iter().all(|x| x.is_finite()));
            }
            // PLC for a sub-frame multiple of 2.5 ms.
            let mut short = vec![0.0; 120 * 2];
            assert_eq!(dec.conceal(&mut short).unwrap(), 120);
            // FEC after a real loss produces non-silent audio when SILK carried LBRR.
            dec.reset();
            assert_eq!(dec.decode(&pkts[10], &mut out).unwrap(), fs);
            assert_eq!(dec.decode_fec(&pkts[12], &mut out).unwrap(), fs);
            assert!(rms(&out) > 0.01, "recovered frame is silent");
        }
    }

    #[test]
    fn rejects_bad_config_and_buffers() {
        let bad = [
            OpusConfig {
                sample_rate: 44_100,
                ..OpusConfig::default()
            },
            OpusConfig {
                channels: 3,
                ..OpusConfig::default()
            },
            OpusConfig {
                bitrate: 1_000,
                ..OpusConfig::default()
            },
            OpusConfig {
                frame_ms: 15,
                ..OpusConfig::default()
            },
            OpusConfig {
                expected_loss_pct: 101,
                ..OpusConfig::default()
            },
        ];
        for cfg in bad {
            assert!(
                matches!(OpusEncoder::new(cfg), Err(AudioError::InvalidConfig(_))),
                "{cfg:?}"
            );
        }
        assert!(matches!(
            OpusDecoder::new(44_100, 2),
            Err(AudioError::InvalidConfig(_))
        ));
        let mut enc = OpusEncoder::new(OpusConfig::default()).unwrap();
        assert_eq!(enc.frame_samples(), 480);
        let mut out = [0_u8; MAX_OPUS_PACKET];
        assert_eq!(
            enc.encode(&[0.0; 100], &mut out),
            Err(AudioError::BufferSize {
                expected: 960,
                got: 100
            })
        );
        let mut dec = OpusDecoder::new(48_000, 2).unwrap();
        assert!(matches!(
            dec.decode(&[], &mut [0.0; 960]),
            Err(AudioError::Opus { .. })
        ));
        // A corrupt packet is an Opus error, not a panic.
        let mut garbage_out = [0.0; 960];
        let r = dec.decode(&[0xFF; 3], &mut garbage_out);
        assert!(
            matches!(r, Err(AudioError::Opus { code, .. }) if code < 0),
            "{r:?}"
        );
        // Output too small for one frame.
        let n = enc.encode(&[0.0; 960], &mut out).unwrap();
        assert!(matches!(
            dec.decode(&out[..n], &mut [0.0; 100]),
            Err(AudioError::Opus { .. })
        ));
        assert!(matches!(
            dec.conceal(&mut [0.0; 101]),
            Err(AudioError::BufferSize { .. })
        ));
        assert!(matches!(
            dec.decode_fec(&out[..n], &mut []),
            Err(AudioError::BufferSize { .. })
        ));
    }

    #[test]
    fn setters_update_config() {
        let mut enc = OpusEncoder::new(OpusConfig::default()).unwrap();
        enc.set_bitrate(64_000).unwrap();
        enc.set_expected_loss(20).unwrap();
        enc.set_fec(false).unwrap();
        assert_eq!(enc.config().bitrate, 64_000);
        assert_eq!(enc.config().expected_loss_pct, 20);
        assert!(!enc.config().fec);
        assert!(matches!(enc.set_bitrate(1), Err(AudioError::Opus { .. })));
        assert!(matches!(
            enc.set_expected_loss(200),
            Err(AudioError::Opus { .. })
        ));
        assert_eq!(enc.config().bitrate, 64_000);
        // Still encodes after reconfiguration.
        let mut out = [0_u8; MAX_OPUS_PACKET];
        assert!(enc.encode(&[0.1; 960], &mut out).unwrap() > 0);
    }
}
