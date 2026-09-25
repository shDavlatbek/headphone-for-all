//! # hfa-capture
//!
//! OS audio I/O for headphone-for-all:
//!
//! - **Capture** ([`CaptureSource`]): system mix / per-process capture per OS, test tone, WAV
//!   file, and "external" feeds pushed from native code (Android, iOS) through FFI.
//! - **Output** ([`AudioOutput`]): playback on the default or a named device (`cpal`), WAV file
//!   and null sinks.
//!
//! Audio leaves/enters these objects only through lock-free SPSC rings ([`pcm_ring`]). OS
//! audio callbacks never allocate, lock, log or make syscalls.
//!
//! Exactly one OS module is compiled as `platform` (linux, windows, macos, or `unsupported`
//! for every other target, including Android and iOS). Each exposes the same four
//! `pub(crate)` functions: `capabilities`, `open_system`, `open_process`, `list_apps`.
//!
//! See `docs/CONTRACTS.md` §5 for the binding contract.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use hfa_audio::AudioFormat;
use serde::{Deserialize, Serialize};

pub mod error;
pub mod external;
pub mod output_cpal;
pub mod output_file;
mod pacer;
pub mod ring;
pub mod tone;
pub mod wav_source;

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod platform;
#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod platform;
#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
#[path = "unsupported.rs"]
mod platform;
#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod platform;

pub use error::CaptureError;
pub use external::{register_external, unregister_external, ExternalFeed};
pub use ring::{pcm_ring, pcm_ring_with_channels, PcmSink, PcmSource, RingStats};

/// Result type used throughout this crate.
pub type Result<T> = std::result::Result<T, CaptureError>;

/// A source of captured audio. Implementations deliver interleaved `f32` in their native
/// [`CaptureSource::format`]; the sender converts it to the internal format.
pub trait CaptureSource: Send {
    /// Human-readable description (e.g. "System audio (PipeWire monitor)").
    fn describe(&self) -> String;
    /// Format of the samples pushed into the sink. Valid before `start`.
    fn format(&self) -> AudioFormat;
    /// Starts capturing into `sink`. The backend owns the sink until `stop`.
    ///
    /// # Errors
    /// Backend-specific [`CaptureError`]s; [`CaptureError::AlreadyRunning`] if started twice.
    fn start(&mut self, sink: PcmSink) -> Result<()>;
    /// Stops capturing and releases OS resources. Idempotent.
    fn stop(&mut self);
}

/// A playback destination. It pulls interleaved `f32` in [`AudioOutput::format`] from the ring.
pub trait AudioOutput: Send {
    /// Format the output expects in the ring. Valid before `start`.
    fn format(&self) -> AudioFormat;
    /// Starts playback, pulling from `source`. Underruns play silence.
    ///
    /// # Errors
    /// Backend-specific [`CaptureError`]s; [`CaptureError::AlreadyRunning`] if started twice.
    fn start(&mut self, source: PcmSource) -> Result<()>;
    /// Stops playback. Idempotent.
    fn stop(&mut self);
    /// Estimated device latency (buffer + hardware) in ms, if known.
    fn latency_ms(&self) -> Option<f32>;
    /// `true` once the output failed while running and no longer consumes or plays the ring
    /// as it should (device unplugged or stream invalidated, WAV write error...). The owner
    /// (the hub) polls this periodically; on `true` it should stop this output and open a new
    /// one, or report the error. Reset by `start`. Glitches (xruns) and route changes the
    /// backend handled itself are not errors.
    fn has_error(&self) -> bool {
        false
    }
    /// Buffer under/overruns (xruns) the backend reported since `start` (0 if unknown).
    fn xruns(&self) -> u64 {
        0
    }
    /// `false` if calling `start` again after a failure would destroy what the output already
    /// produced (a WAV file is truncated by `start`). The hub then does not reopen the output
    /// after [`AudioOutput::has_error`] and only reports the error. Default `true` (devices).
    fn restartable(&self) -> bool {
        true
    }
}

/// What to capture.
///
/// String form (used by the CLI and settings; see [`FromStr`]/[`fmt::Display`]):
/// `system`, `system-excl`, `pid:<n>`, `tone:<hz>`, `wav:<path>`, `external:<id>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CaptureTarget {
    /// Everything the device plays.
    SystemMix,
    /// Everything the device plays except this process (so a hub+sender never loops).
    SystemMixExcludingSelf,
    /// One process (and its child processes where the OS supports it).
    Process {
        /// Process id.
        pid: u32,
    },
    /// A generated sine tone at 48 kHz stereo (tests, selftest).
    Tone {
        /// Frequency in Hz.
        freq_hz: f32,
    },
    /// A WAV file, played in real time and looped.
    WavFile(PathBuf),
    /// PCM pushed from native code through [`register_external`] (Android, iOS).
    External {
        /// Feed id passed to [`register_external`].
        id: u32,
    },
}

/// A process that is currently playing audio and can be captured on its own.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CaptureApp {
    /// Process id.
    pub pid: u32,
    /// Executable or display name.
    pub name: String,
}

/// What capture can do on this OS/build.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Capabilities {
    /// [`CaptureTarget::SystemMix`] / [`CaptureTarget::SystemMixExcludingSelf`] work.
    pub system_mix: bool,
    /// [`CaptureTarget::Process`] and [`list_capture_apps`] work.
    pub per_app: bool,
    /// Capturing silences the local speakers (e.g. macOS taps with `mutedWhenTapped`).
    pub mutes_local_output: bool,
    /// Human-readable caveats (permissions, OS version requirements...).
    pub notes: String,
}

/// Where the hub plays the mix.
///
/// String form (see [`FromStr`]/[`fmt::Display`]): `default`, `device:<name>`,
/// `wav:<path>`, `null`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum OutputTarget {
    /// The OS default output device (usually the headphone).
    #[default]
    Default,
    /// A named output device (see [`list_output_devices`]).
    Device(String),
    /// Write the mix to a WAV file, paced in real time.
    WavFile(PathBuf),
    /// Discard the mix, paced in real time.
    Null,
}

/// Capture capabilities of this platform.
pub fn capabilities() -> Capabilities {
    platform::capabilities()
}

/// Processes that can be captured individually. `Ok(vec![])` on platforms without per-app
/// capture.
///
/// # Errors
/// Backend errors while enumerating audio sessions.
pub fn list_capture_apps() -> Result<Vec<CaptureApp>> {
    platform::list_apps()
}

/// Opens (but does not start) a capture source.
///
/// # Errors
/// [`CaptureError::Unsupported`] if the target is not available on this platform, or
/// backend errors.
pub fn open_capture(target: &CaptureTarget) -> Result<Box<dyn CaptureSource>> {
    match target {
        CaptureTarget::SystemMix => platform::open_system(false),
        CaptureTarget::SystemMixExcludingSelf => platform::open_system(true),
        CaptureTarget::Process { pid } => platform::open_process(*pid),
        CaptureTarget::Tone { freq_hz } => Ok(Box::new(tone::ToneSource::new(
            *freq_hz,
            AudioFormat::INTERNAL,
        ))),
        CaptureTarget::WavFile(path) => Ok(Box::new(wav_source::WavFileSource::open(path)?)),
        CaptureTarget::External { id } => Ok(Box::new(external::ExternalSource::open(*id)?)),
    }
}

/// Opens (but does not start) an output. `buffer_ms` is the desired device buffer size (the
/// pull period for WAV and null outputs). Device outputs use 48 kHz when the device supports
/// it, else the device's default rate, and the device's channel count (see
/// [`AudioOutput::format`]; the hub resamples to it); WAV and null outputs use
/// [`AudioFormat::INTERNAL`].
///
/// # Errors
/// [`CaptureError::NotFound`] for an unknown device, or backend errors.
pub fn open_output(target: &OutputTarget, buffer_ms: u32) -> Result<Box<dyn AudioOutput>> {
    match target {
        OutputTarget::Default => Ok(Box::new(output_cpal::CpalOutput::open(None, buffer_ms)?)),
        OutputTarget::Device(name) => Ok(Box::new(output_cpal::CpalOutput::open(
            Some(name),
            buffer_ms,
        )?)),
        OutputTarget::WavFile(path) => Ok(Box::new(output_file::WavFileOutput::create(
            path,
            AudioFormat::INTERNAL,
            buffer_ms,
        )?)),
        OutputTarget::Null => Ok(Box::new(output_file::NullOutput::new(
            AudioFormat::INTERNAL,
            buffer_ms,
        ))),
    }
}

/// Names of the available output devices (for [`OutputTarget::Device`]).
///
/// # Errors
/// Backend errors.
pub fn list_output_devices() -> Result<Vec<String>> {
    output_cpal::list_devices()
}

fn split_kind(s: &str) -> (&str, Option<&str>) {
    match s.split_once(':') {
        Some((kind, rest)) => (kind, Some(rest)),
        None => (s, None),
    }
}

impl FromStr for CaptureTarget {
    type Err = CaptureError;

    fn from_str(s: &str) -> Result<Self> {
        let bad = |why: &str| CaptureError::InvalidArgument(format!("capture target {s:?}: {why}"));
        let (kind, rest) = split_kind(s.trim());
        match (kind, rest) {
            ("system", None) => Ok(CaptureTarget::SystemMix),
            ("system-excl", None) => Ok(CaptureTarget::SystemMixExcludingSelf),
            ("pid", Some(n)) => n
                .parse()
                .map(|pid| CaptureTarget::Process { pid })
                .map_err(|_| bad("pid must be an unsigned integer")),
            ("tone", Some(hz)) => match hz.parse::<f32>() {
                Ok(freq_hz) if freq_hz.is_finite() && freq_hz > 0.0 && freq_hz < 24_000.0 => {
                    Ok(CaptureTarget::Tone { freq_hz })
                }
                _ => Err(bad("tone frequency must be a number in (0, 24000) Hz")),
            },
            ("wav", Some(path)) if !path.is_empty() => Ok(CaptureTarget::WavFile(path.into())),
            ("external", Some(id)) => id
                .parse()
                .map(|id| CaptureTarget::External { id })
                .map_err(|_| bad("external id must be an unsigned integer")),
            _ => Err(bad(
                "expected system | system-excl | pid:<n> | tone:<hz> | wav:<path> | external:<id>",
            )),
        }
    }
}

impl fmt::Display for CaptureTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureTarget::SystemMix => f.write_str("system"),
            CaptureTarget::SystemMixExcludingSelf => f.write_str("system-excl"),
            CaptureTarget::Process { pid } => write!(f, "pid:{pid}"),
            CaptureTarget::Tone { freq_hz } => write!(f, "tone:{freq_hz}"),
            CaptureTarget::WavFile(path) => write!(f, "wav:{}", path.display()),
            CaptureTarget::External { id } => write!(f, "external:{id}"),
        }
    }
}

impl FromStr for OutputTarget {
    type Err = CaptureError;

    fn from_str(s: &str) -> Result<Self> {
        let (kind, rest) = split_kind(s.trim());
        match (kind, rest) {
            ("default", None) => Ok(OutputTarget::Default),
            ("null", None) => Ok(OutputTarget::Null),
            ("device", Some(name)) if !name.is_empty() => Ok(OutputTarget::Device(name.into())),
            ("wav", Some(path)) if !path.is_empty() => Ok(OutputTarget::WavFile(path.into())),
            _ => Err(CaptureError::InvalidArgument(format!(
                "output target {s:?}: expected default | device:<name> | wav:<path> | null"
            ))),
        }
    }
}

impl fmt::Display for OutputTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OutputTarget::Default => f.write_str("default"),
            OutputTarget::Device(name) => write!(f, "device:{name}"),
            OutputTarget::WavFile(path) => write!(f, "wav:{}", path.display()),
            OutputTarget::Null => f.write_str("null"),
        }
    }
}

// Compile-time guarantees: feeds are shared with FFI threads; rings move into callbacks.
const _: () = {
    const fn assert_send<T: Send>() {}
    const fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ExternalFeed>();
    assert_send::<PcmSink>();
    assert_send::<PcmSource>();
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_target_round_trips() {
        let cases = [
            CaptureTarget::SystemMix,
            CaptureTarget::SystemMixExcludingSelf,
            CaptureTarget::Process { pid: 4242 },
            CaptureTarget::Tone { freq_hz: 440.0 },
            CaptureTarget::WavFile(PathBuf::from("/tmp/a b.wav")),
            CaptureTarget::External { id: 7 },
        ];
        for t in cases {
            let parsed: CaptureTarget = t.to_string().parse().expect("round trip");
            assert_eq!(parsed, t);
        }
    }

    #[test]
    fn capture_target_rejects_garbage() {
        for s in [
            "", "sys", "pid:", "pid:-1", "tone:abc", "tone:0", "tone:NaN", "wav:", "system:1",
        ] {
            assert!(
                s.parse::<CaptureTarget>().is_err(),
                "{s:?} should be rejected"
            );
        }
    }

    #[test]
    fn output_target_round_trips() {
        let cases = [
            OutputTarget::Default,
            OutputTarget::Null,
            OutputTarget::Device("USB Headset: Analog".into()),
            OutputTarget::WavFile(PathBuf::from("out.wav")),
        ];
        for t in cases {
            let parsed: OutputTarget = t.to_string().parse().expect("round trip");
            assert_eq!(parsed, t);
        }
        assert!("speaker".parse::<OutputTarget>().is_err());
        assert!("device:".parse::<OutputTarget>().is_err());
        assert_eq!(OutputTarget::default(), OutputTarget::Default);
    }
}
