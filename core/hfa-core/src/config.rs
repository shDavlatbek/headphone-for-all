//! Persisted user settings (`<data_dir>/settings.json`).

use std::path::{Path, PathBuf};

use hfa_capture::OutputTarget;
use serde::{Deserialize, Serialize};

use crate::Result;

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
    /// Directory holding settings, identity and trust store. Not serialized: it is where the
    /// file was loaded from.
    #[serde(skip)]
    pub data_dir: PathBuf,
}

impl Default for Settings {
    /// Port 47810, 128 kbit/s, 10 ms, FEC on, jitter 20..=150 ms, default output,
    /// device name from the host name, data dir = [`default_data_dir`] (or `.` if unknown).
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
            data_dir: default_data_dir().unwrap_or_else(|| PathBuf::from(".")),
        }
    }
}

impl Settings {
    /// Loads `<dir>/settings.json`, or returns defaults (with `data_dir = dir`) if the file does
    /// not exist. Creates `dir` if needed.
    ///
    /// # Errors
    /// [`crate::CoreError::Io`] / [`crate::CoreError::Json`] for unreadable or corrupt files.
    pub fn load_or_default(_dir: &Path) -> Result<Settings> {
        todo!("feat/core-engine")
    }

    /// Writes `<data_dir>/settings.json` atomically (write temp file + rename).
    ///
    /// # Errors
    /// [`crate::CoreError::Io`] / [`crate::CoreError::Json`].
    pub fn save(&self) -> Result<()> {
        todo!("feat/core-engine")
    }
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
    }
}
