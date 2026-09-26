//! The hub's remembered per-device controls (gain, mute, priority), persisted in
//! `<data_dir>/`[`HUB_CONTROLS_FILE`] so they survive hub and app restarts.
//!
//! Format: `{"version":1,"devices":{"<device id>":{"gain":0.5,"muted":false,"priority":true}}}`,
//! rewritten atomically (temp file + rename). A missing file is an empty map; an unreadable or
//! corrupt one is logged and ignored (the hub still starts, and the next change rewrites it).
//! Entries of devices that are no longer trusted are dropped on load and on save.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use hfa_audio::mixer::MAX_GAIN;
use serde::{Deserialize, Serialize};

use crate::config::write_file_atomic;
use crate::hub_mixer::Controls;
use crate::identity::TrustStore;
use crate::Result;

/// File name of the remembered controls inside the data directory.
pub const HUB_CONTROLS_FILE: &str = "hub_controls.json";
/// Version of the file format.
const VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    #[serde(default)]
    devices: HashMap<String, Entry>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    gain: f32,
    #[serde(default)]
    muted: bool,
    #[serde(default)]
    priority: bool,
}

/// Path of the file in `data_dir`.
pub(crate) fn path(data_dir: &Path) -> PathBuf {
    data_dir.join(HUB_CONTROLS_FILE)
}

/// Loads the remembered controls of trusted devices (blocking file I/O). Never fails: a
/// missing file is empty, a broken one is logged and ignored.
pub(crate) fn load(data_dir: &Path, trust: &TrustStore) -> HashMap<String, Controls> {
    let path = path(data_dir);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return HashMap::new(),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "cannot read the remembered hub controls");
            return HashMap::new();
        }
    };
    let file = match serde_json::from_slice::<File>(&bytes) {
        Ok(f) if f.version == VERSION => f,
        Ok(f) => {
            tracing::warn!(
                version = f.version,
                "ignoring hub controls of an unknown version"
            );
            return HashMap::new();
        }
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "ignoring corrupt hub controls");
            return HashMap::new();
        }
    };
    file.devices
        .into_iter()
        .filter(|(id, _)| trust.get(id).is_some())
        .map(|(id, e)| {
            let gain = if e.gain.is_finite() {
                e.gain.clamp(0.0, MAX_GAIN)
            } else {
                1.0
            };
            (
                id,
                Controls {
                    gain,
                    muted: e.muted,
                    priority: e.priority,
                },
            )
        })
        .collect()
}

/// Saves the controls of trusted devices (blocking file I/O).
pub(crate) fn save(
    data_dir: &Path,
    prefs: &HashMap<String, Controls>,
    trust: &TrustStore,
) -> Result<()> {
    let file = File {
        version: VERSION,
        devices: prefs
            .iter()
            .filter(|(id, _)| trust.get(id).is_some())
            .map(|(id, c)| {
                (
                    id.clone(),
                    Entry {
                        gain: c.gain,
                        muted: c.muted,
                        priority: c.priority,
                    },
                )
            })
            .collect(),
    };
    let json = serde_json::to_vec_pretty(&file)?;
    write_file_atomic(&path(data_dir), &json, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::TrustedPeer;

    #[test]
    fn round_trip_keeps_only_trusted_devices() {
        let dir = tempfile::tempdir().expect("tempdir");
        let trust = TrustStore::load(dir.path()).expect("trust");
        let peer = TrustedPeer::new([3; 32], "Laptop");
        trust.add(peer.clone()).expect("add");
        let mut prefs = HashMap::new();
        let c = Controls {
            gain: 0.5,
            muted: true,
            priority: true,
        };
        prefs.insert(peer.device_id.clone(), c);
        prefs.insert("not-trusted".to_owned(), Controls::default());
        save(dir.path(), &prefs, &trust).expect("save");
        let loaded = load(dir.path(), &trust);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.get(&peer.device_id), Some(&c));
        // A forgotten device's controls are dropped.
        trust.remove(&peer.device_id).expect("remove");
        assert!(load(dir.path(), &trust).is_empty());
    }

    #[test]
    fn broken_files_are_ignored() {
        let dir = tempfile::tempdir().expect("tempdir");
        let trust = TrustStore::load(dir.path()).expect("trust");
        assert!(load(dir.path(), &trust).is_empty(), "missing file");
        let peer = TrustedPeer::new([4; 32], "Phone");
        trust.add(peer.clone()).expect("add");
        std::fs::write(path(dir.path()), b"{not json").expect("write");
        assert!(load(dir.path(), &trust).is_empty());
        std::fs::write(path(dir.path()), br#"{"version":9,"devices":{}}"#).expect("write");
        assert!(load(dir.path(), &trust).is_empty());
        // Out-of-range values are clamped.
        let json = format!(
            r#"{{"version":1,"devices":{{"{}":{{"gain":99.0}}}}}}"#,
            peer.device_id
        );
        std::fs::write(path(dir.path()), json).expect("write");
        let loaded = load(dir.path(), &trust);
        assert_eq!(loaded[&peer.device_id].gain, MAX_GAIN);
    }
}
