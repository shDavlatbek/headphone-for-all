//! Persisted user settings (`<data_dir>/settings.json`).

use std::io::Write as _;
use std::path::{Path, PathBuf};

use hfa_capture::OutputTarget;
use serde::{Deserialize, Serialize};

use crate::{CoreError, Result};

/// File name of the settings inside the data directory.
pub const SETTINGS_FILE: &str = "settings.json";

/// User settings shared by the hub and sender engines.
///
/// Unknown/missing JSON fields fall back to [`Settings::default`] values (`#[serde(default)]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Name shown to other devices.
    pub device_name: String,
    /// TCP control / UDP media port of the hub (senders: default port of `--to` hosts).
    pub port: u16,
    /// Initial Opus bitrate in bits per second (sender).
    pub bitrate: u32,
    /// Opus frame duration in ms, 10 or 20 (sender).
    pub frame_ms: u32,
    /// Opus in-band FEC (sender).
    pub fec: bool,
    /// Minimum jitter-buffer target in ms (hub).
    pub jitter_min_ms: u32,
    /// Maximum jitter-buffer target in ms (hub).
    pub jitter_max_ms: u32,
    /// Where the hub plays the mix.
    pub output: OutputTarget,
    /// The hub's master linear gain, [`MIN_MASTER_GAIN`]`..=`[`MAX_MASTER_GAIN`] (1.0 =
    /// unchanged). `HubEngine::start` applies it, so the listener's volume survives hub and
    /// app restarts; the app saves it whenever the master slider moves (a missing field in
    /// older files means 1.0).
    pub master_gain: f32,
    /// Directory holding settings, identity and trust store. Not serialized: it is where the
    /// file was loaded from.
    #[serde(skip)]
    pub data_dir: PathBuf,
}

impl Default for Settings {
    /// Port 47810, 128 kbit/s, 10 ms, FEC on, jitter 20..=150 ms, default output, master
    /// gain 1.0, device name from the host name, data dir = [`default_data_dir`] (or `.` if unknown).
    fn default() -> Self {
        Self {
            device_name: default_device_name(),
            port: hfa_proto::DEFAULT_PORT,
            bitrate: 128_000,
            frame_ms: 10,
            fec: true,
            jitter_min_ms: 20,
            jitter_max_ms: 150,
            output: OutputTarget::Default,
            master_gain: 1.0,
            data_dir: default_data_dir().unwrap_or_else(|| PathBuf::from(".")),
        }
    }
}

/// Smallest accepted Opus bitrate (bits per second).
pub const MIN_BITRATE: u32 = 6_000;
/// Largest accepted Opus bitrate (bits per second).
pub const MAX_BITRATE: u32 = 510_000;
/// Opus frame durations the engines support (ms).
pub const FRAME_MS_CHOICES: [u32; 2] = [10, 20];
/// Upper bound for the jitter-buffer targets (ms).
pub const MAX_JITTER_MS: u32 = 2_000;
/// Smallest accepted master gain (silence).
pub const MIN_MASTER_GAIN: f32 = 0.0;
/// Largest accepted master gain (+12 dB, the mixer's `MAX_GAIN`).
pub const MAX_MASTER_GAIN: f32 = hfa_audio::mixer::MAX_GAIN;
/// Longest accepted device name in bytes (same limit as the pairing URI's `n` parameter).
pub const MAX_DEVICE_NAME_LEN: usize = hfa_proto::uri::MAX_NAME_LEN;

impl Settings {
    /// Loads `<dir>/settings.json`, or returns defaults (with `data_dir = dir`) if the file does
    /// not exist. Creates `dir` if needed. The loaded settings are [validated](Self::validate).
    ///
    /// # Errors
    /// [`crate::CoreError::Io`] / [`crate::CoreError::Json`] for unreadable or corrupt files,
    /// [`crate::CoreError::Config`] if the file holds invalid values.
    pub fn load_or_default(dir: &Path) -> Result<Settings> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(SETTINGS_FILE);
        let mut settings = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice::<Settings>(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(path = %path.display(), "no settings file, using defaults");
                Settings::default()
            }
            Err(e) => return Err(e.into()),
        };
        settings.data_dir = dir.to_path_buf();
        settings.validate()?;
        Ok(settings)
    }

    /// Writes `<data_dir>/settings.json` atomically (write temp file + rename). Creates
    /// `data_dir` if needed. Invalid settings are refused (nothing is written).
    ///
    /// # Errors
    /// [`crate::CoreError::Config`] (see [`Settings::validate`]),
    /// [`crate::CoreError::Io`] / [`crate::CoreError::Json`].
    pub fn save(&self) -> Result<()> {
        self.validate()?;
        let json = serde_json::to_vec_pretty(self)?;
        write_file_atomic(&self.data_dir.join(SETTINGS_FILE), &json, false)
    }

    /// Checks every field:
    /// - `device_name`: not blank, at most [`MAX_DEVICE_NAME_LEN`] bytes, no control characters;
    /// - `frame_ms` ∈ [`FRAME_MS_CHOICES`] (10 or 20);
    /// - `bitrate` in [`MIN_BITRATE`]`..=`[`MAX_BITRATE`];
    /// - `1 <= jitter_min_ms <= jitter_max_ms <=` [`MAX_JITTER_MS`];
    /// - `master_gain` finite and in [`MIN_MASTER_GAIN`]`..=`[`MAX_MASTER_GAIN`].
    ///
    /// Every `port` is valid: `0` lets the hub bind any free port (see `HubHandle::local_port`).
    ///
    /// # Errors
    /// [`crate::CoreError::Config`] naming the first invalid field.
    pub fn validate(&self) -> Result<()> {
        let name = self.device_name.trim();
        if name.is_empty() {
            return Err(config_err("device_name must not be empty"));
        }
        if self.device_name.len() > MAX_DEVICE_NAME_LEN {
            return Err(config_err(format!(
                "device_name is longer than {MAX_DEVICE_NAME_LEN} bytes"
            )));
        }
        if self.device_name.chars().any(char::is_control) {
            return Err(config_err("device_name contains control characters"));
        }
        if !FRAME_MS_CHOICES.contains(&self.frame_ms) {
            return Err(config_err(format!(
                "frame_ms must be 10 or 20, got {}",
                self.frame_ms
            )));
        }
        if !(MIN_BITRATE..=MAX_BITRATE).contains(&self.bitrate) {
            return Err(config_err(format!(
                "bitrate must be in {MIN_BITRATE}..={MAX_BITRATE}, got {}",
                self.bitrate
            )));
        }
        if self.jitter_min_ms == 0 || self.jitter_min_ms > self.jitter_max_ms {
            return Err(config_err(format!(
                "jitter bounds must satisfy 1 <= min <= max, got {}..={}",
                self.jitter_min_ms, self.jitter_max_ms
            )));
        }
        if self.jitter_max_ms > MAX_JITTER_MS {
            return Err(config_err(format!(
                "jitter_max_ms must be at most {MAX_JITTER_MS}, got {}",
                self.jitter_max_ms
            )));
        }
        if !self.master_gain.is_finite()
            || !(MIN_MASTER_GAIN..=MAX_MASTER_GAIN).contains(&self.master_gain)
        {
            return Err(config_err(format!(
                "master_gain must be in {MIN_MASTER_GAIN}..={MAX_MASTER_GAIN}, got {}",
                self.master_gain
            )));
        }
        Ok(())
    }
}

fn config_err(msg: impl Into<String>) -> CoreError {
    CoreError::Config(msg.into())
}

/// Replaces `path` with `bytes` atomically: the data goes to a fresh temporary file in the
/// same directory (created with `create_new`, unix mode 0600 when `private`, else 0644, both
/// reduced by the umask), is flushed to disk and then renamed over `path`. Readers see either
/// the old or the new complete file, never a partial one. Creates the parent directory.
pub(crate) fn write_file_atomic(path: &Path, bytes: &[u8], private: bool) -> Result<()> {
    let tmp = write_temp_file(path, bytes, private)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    sync_parent_dir(path);
    Ok(())
}

/// Writes `bytes` into a new temporary file next to `path` (see [`write_file_atomic`]) and
/// returns its path. The file is fully written and synced; the caller renames or links it.
pub(crate) fn write_temp_file(path: &Path, bytes: &[u8], private: bool) -> Result<PathBuf> {
    let dir = match path.parent() {
        Some(d) if !d.as_os_str().is_empty() => d,
        _ => Path::new("."),
    };
    std::fs::create_dir_all(dir)?;
    let base = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let tmp = dir.join(format!(".{base}.{:016x}.tmp", rand::random::<u64>()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // The mode applies at creation, so a private file is never readable by others, not
        // even between creation and a later chmod.
        options.mode(if private { 0o600 } else { 0o644 });
    }
    #[cfg(not(unix))]
    let _ = private;
    let result = options.open(&tmp).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(tmp)
}

/// Flushes the directory entry of a rename (best effort; unix only).
fn sync_parent_dir(path: &Path) {
    #[cfg(unix)]
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Platform data directory for headphone-for-all (via `directories`), e.g.
/// `~/.local/share/headphone-for-all` on Linux. `None` if no home directory is known
/// (mobile apps pass their own directory from Flutter).
pub fn default_data_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("io.github", "shdavlatbek", "headphone-for-all")
        .map(|d| d.data_dir().to_path_buf())
}

/// The name this device shows to other devices by default.
///
/// Order: the OS host name (`gethostname` on unix, `COMPUTERNAME` on Windows), then the
/// `HOSTNAME` / `COMPUTERNAME` / `HOST` environment variables, then `hfa-XXXX` with four random
/// hex digits (so unnamed devices can still be told apart; the value is persisted by
/// [`Settings::save`]). A trailing `.local` is stripped and `localhost` is ignored (Android
/// and iOS report `localhost`; the Flutter app sets [`Settings::device_name`] to the device
/// model there).
pub fn default_device_name() -> String {
    os_host_name()
        .or_else(|| {
            ["HOSTNAME", "COMPUTERNAME", "HOST"]
                .iter()
                .filter_map(|k| std::env::var(k).ok())
                .find_map(|s| clean_host_name(&s))
        })
        .unwrap_or_else(|| format!("hfa-{:04x}", rand::random::<u16>()))
}

/// Normalizes a host name for display; `None` if it is empty or `localhost`.
fn clean_host_name(raw: &str) -> Option<String> {
    let name = raw.trim().trim_end_matches('.');
    let name = name
        .strip_suffix(".local")
        .or_else(|| name.strip_suffix(".localdomain"))
        .unwrap_or(name)
        .trim();
    if name.is_empty() || name.eq_ignore_ascii_case("localhost") {
        None
    } else {
        Some(name.to_owned())
    }
}

/// The host name reported by the OS.
#[cfg(unix)]
fn os_host_name() -> Option<String> {
    // POSIX host names are at most 255 bytes (HOST_NAME_MAX); one extra byte for the NUL.
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is valid for writes of `buf.len()` bytes; gethostname writes at most that
    // many bytes and we never read past the buffer (the NUL search is bounded by its length).
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast::<libc::c_char>(), buf.len()) };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    clean_host_name(&String::from_utf8_lossy(&buf[..end]))
}

/// The host name reported by the OS (`COMPUTERNAME` is set by Windows for every process).
#[cfg(windows)]
fn os_host_name() -> Option<String> {
    std::env::var("COMPUTERNAME")
        .ok()
        .and_then(|s| clean_host_name(&s))
}

/// No OS host name source on this target.
#[cfg(not(any(unix, windows)))]
fn os_host_name() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_contract() {
        let s = Settings::default();
        assert_eq!(s.port, 47810);
        assert_eq!(s.frame_ms, 10);
        assert!(s.fec);
        assert!(s.jitter_min_ms < s.jitter_max_ms);
        assert!(!s.device_name.is_empty());
    }

    #[test]
    fn host_name_cleanup() {
        assert_eq!(clean_host_name(" desk \n").as_deref(), Some("desk"));
        assert_eq!(
            clean_host_name("MacBook-Pro.local").as_deref(),
            Some("MacBook-Pro")
        );
        assert_eq!(
            clean_host_name("MacBook-Pro.local.").as_deref(),
            Some("MacBook-Pro")
        );
        assert_eq!(clean_host_name("box.localdomain").as_deref(), Some("box"));
        assert_eq!(
            clean_host_name("desk.example.org").as_deref(),
            Some("desk.example.org")
        );
        assert_eq!(clean_host_name("localhost"), None);
        assert_eq!(clean_host_name("LOCALHOST"), None);
        assert_eq!(clean_host_name("   "), None);
        assert_eq!(clean_host_name(".local"), None);
    }

    /// The device name must come from the kernel, not from `HOSTNAME` (which shells do not
    /// export, and GUI/mobile processes never see).
    #[cfg(target_os = "linux")]
    #[test]
    fn device_name_is_the_kernel_host_name() {
        let kernel = std::fs::read_to_string("/proc/sys/kernel/hostname").expect("hostname");
        if let Some(expected) = clean_host_name(&kernel) {
            assert_eq!(default_device_name(), expected);
        } else {
            assert!(default_device_name().starts_with("hfa-"));
        }
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        let s: Settings =
            serde_json::from_str(r#"{"device_name":"desk","port":5000}"#).expect("parse");
        assert_eq!(s.device_name, "desk");
        assert_eq!(s.port, 5000);
        assert_eq!(s.bitrate, Settings::default().bitrate);
        assert_eq!(s.output, OutputTarget::Default);
        // Files written before the master gain was persisted mean unity gain.
        assert!((s.master_gain - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn master_gain_round_trips_and_is_validated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut s = Settings::load_or_default(dir.path()).expect("defaults");
        assert!((s.master_gain - 1.0).abs() < f32::EPSILON);
        s.master_gain = 0.35;
        s.save().expect("save");
        let loaded = Settings::load_or_default(dir.path()).expect("load");
        assert!((loaded.master_gain - 0.35).abs() < 1e-6);

        for bad in [-0.1, MAX_MASTER_GAIN + 0.1, f32::NAN, f32::INFINITY] {
            let mut invalid = loaded.clone();
            invalid.master_gain = bad;
            assert!(
                matches!(invalid.validate(), Err(CoreError::Config(_))),
                "{bad}"
            );
            assert!(invalid.save().is_err(), "{bad} must not be saved");
        }
        // The rejected saves left the file alone.
        let again = Settings::load_or_default(dir.path()).expect("load");
        assert!((again.master_gain - 0.35).abs() < 1e-6);
        for ok in [MIN_MASTER_GAIN, 1.0, MAX_MASTER_GAIN] {
            let mut valid = again.clone();
            valid.master_gain = ok;
            valid.validate().expect("valid gain");
        }
    }
}
