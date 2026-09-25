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

/// Host name from the environment (`HOSTNAME` / `COMPUTERNAME`), or `"hfa-device"`.
pub fn default_device_name() -> String {
    ["HOSTNAME", "COMPUTERNAME"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .map(|s| s.trim().to_owned())
        .find(|s| !s.is_empty())
        .unwrap_or_else(|| "hfa-device".to_owned())
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
    fn missing_fields_fall_back_to_defaults() {
        let s: Settings =
            serde_json::from_str(r#"{"device_name":"desk","port":5000}"#).expect("parse");
        assert_eq!(s.device_name, "desk");
        assert_eq!(s.port, 5000);
        assert_eq!(s.bitrate, Settings::default().bitrate);
        assert_eq!(s.output, OutputTarget::Default);
    }
}
