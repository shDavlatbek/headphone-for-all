//! Device identity (Noise static keypair) and the store of trusted (paired) peers.
//!
//! Files in the data directory:
//! - [`IDENTITY_FILE`] (`identity.json`): `{"version":1,"private_key":"<b64>","public_key":"<b64>"}`
//!   (standard base64 with padding). On unix it is created with mode `0600` directly (the
//!   temporary file is created with that mode, so the private key is never readable by other
//!   users, not even briefly), and an existing file with a wider mode is tightened on load.
//! - [`TRUST_FILE`] (`trusted.json`): `{"version":1,"peers":[{"device_id":..,"name":..,
//!   "public_key":"<b64>","paired_at":<unix s>}]}`, rewritten atomically (temp file + rename)
//!   on every change, mode `0600` on unix.
//!
//! The `device_id` is always [`hfa_proto::fingerprint`] of the static public key.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use hfa_proto::StaticKeypair;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::config::{write_file_atomic, write_temp_file};
use crate::{CoreError, Result};

/// File name of the identity inside the data directory (unix mode 0600).
pub const IDENTITY_FILE: &str = "identity.json";
/// File name of the trust store inside the data directory.
pub const TRUST_FILE: &str = "trusted.json";
/// Version of the identity and trust file formats.
pub const FILE_FORMAT_VERSION: u32 = 1;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// This device's long-term identity. `Debug` never prints the private key.
#[derive(Debug, Clone)]
pub struct Identity {
    /// Noise static keypair.
    pub keypair: StaticKeypair,
    /// [`hfa_proto::fingerprint`] of the public key.
    pub device_id: String,
    /// Display name.
    pub name: String,
}

/// On-disk form of the identity.
#[derive(Serialize, Deserialize)]
struct IdentityFile {
    version: u32,
    private_key: String,
    public_key: String,
}

impl Identity {
    /// Loads `<data_dir>/identity.json`, or creates a new keypair and saves it (unix mode
    /// 0600). `name` becomes the display name (it is not persisted in the identity file).
    /// Creates `data_dir` if needed.
    ///
    /// A corrupt identity file is an error (it is never silently replaced, which would
    /// invalidate every pairing). If two processes create the identity at the same time,
    /// both end up with the one that reached the disk first.
    ///
    /// # Errors
    /// [`crate::CoreError::Io`] / [`crate::CoreError::Json`] / [`crate::CoreError::Proto`]
    /// (key generation), [`crate::CoreError::Config`] for a file with invalid keys.
    pub fn load_or_create(data_dir: &Path, name: &str) -> Result<Identity> {
        std::fs::create_dir_all(data_dir)?;
        let path = data_dir.join(IDENTITY_FILE);
        let keypair = match read_identity(&path)? {
            Some(kp) => kp,
            None => create_identity(&path)?,
        };
        Ok(Identity {
            device_id: hfa_proto::fingerprint(&keypair.public),
            keypair,
            name: name.to_owned(),
        })
    }

    /// The static public key.
    pub fn public_key(&self) -> [u8; 32] {
        self.keypair.public
    }
}

/// Reads the identity file; `Ok(None)` if it does not exist.
fn read_identity(path: &Path) -> Result<Option<StaticKeypair>> {
    let bytes = match std::fs::read(path) {
        Ok(b) => Zeroizing::new(b),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    restrict_permissions(path)?;
    let file: IdentityFile = serde_json::from_slice(&bytes)?;
    parse_identity_file(file).map(Some)
}

/// Validates the on-disk form and builds the keypair.
fn parse_identity_file(file: IdentityFile) -> Result<StaticKeypair> {
    let private_b64 = Zeroizing::new(file.private_key);
    if file.version != FILE_FORMAT_VERSION {
        return Err(CoreError::Config(format!(
            "unsupported identity file version {}",
            file.version
        )));
    }
    let private = Zeroizing::new(decode_key(&private_b64, "private_key")?);
    let public = decode_key(&file.public_key, "public_key")?;
    if public == [0; 32] || *private == [0; 32] {
        return Err(CoreError::Config(
            "identity file holds an all-zero key".into(),
        ));
    }
    Ok(StaticKeypair {
        private: *private,
        public,
    })
}

/// Generates a keypair and stores it at `path` without ever overwriting an existing file.
fn create_identity(path: &Path) -> Result<StaticKeypair> {
    let keypair = StaticKeypair::generate()?;
    let file = IdentityFile {
        version: FILE_FORMAT_VERSION,
        private_key: B64.encode(keypair.private),
        public_key: B64.encode(keypair.public),
    };
    let json = Zeroizing::new(serde_json::to_vec_pretty(&file)?);
    drop(Zeroizing::new(file.private_key));
    let tmp = write_temp_file(path, &json, true)?;
    // `hard_link` fails if `path` exists, so a concurrently created identity is never
    // replaced. Fall back to a checked rename where hard links are not supported.
    let linked = match std::fs::hard_link(&tmp, path) {
        Ok(()) => {
            let _ = std::fs::remove_file(&tmp);
            true
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = std::fs::remove_file(&tmp);
            false
        }
        Err(_) if !path.exists() => {
            if let Err(e) = std::fs::rename(&tmp, path) {
                let _ = std::fs::remove_file(&tmp);
                return Err(e.into());
            }
            true
        }
        Err(_) => {
            let _ = std::fs::remove_file(&tmp);
            false
        }
    };
    if linked {
        tracing::info!(device_id = %hfa_proto::fingerprint(&keypair.public), "created a new device identity");
        return Ok(keypair);
    }
    // Another process won the race: use its identity.
    read_identity(path)?
        .ok_or_else(|| CoreError::Io(format!("identity file {} vanished", path.display())))
}

/// Makes sure the identity file is only accessible by its owner (unix).
fn restrict_permissions(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(path)?;
        if meta.permissions().mode() & 0o077 != 0 {
            tracing::warn!(path = %path.display(), "identity file was accessible by other users; restricting it to 0600");
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn decode_key(b64: &str, what: &str) -> Result<[u8; 32]> {
    let bytes = Zeroizing::new(
        B64.decode(b64.trim())
            .map_err(|e| CoreError::Config(format!("{what}: invalid base64: {e}")))?,
    );
    let mut key = [0u8; 32];
    if bytes.len() != key.len() {
        return Err(CoreError::Config(format!(
            "{what}: expected 32 bytes, got {}",
            bytes.len()
        )));
    }
    key.copy_from_slice(&bytes);
    Ok(key)
}

/// Serde helper: a 32-byte key as standard base64.
mod b64_key {
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(key: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&super::B64.encode(key))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let text = String::deserialize(d)?;
        let bytes = super::B64
            .decode(text.trim())
            .map_err(serde::de::Error::custom)?;
        bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("expected a 32-byte key"))
    }
}

/// A paired peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedPeer {
    /// Fingerprint of `public_key`.
    pub device_id: String,
    /// Display name at pairing time.
    pub name: String,
    /// Pinned Noise static public key (standard base64 in JSON).
    #[serde(with = "b64_key")]
    pub public_key: [u8; 32],
    /// Unix time (seconds) of pairing.
    pub paired_at: u64,
}

impl TrustedPeer {
    /// A peer with `device_id` derived from `public_key` and `paired_at` = now.
    pub fn new(public_key: [u8; 32], name: impl Into<String>) -> Self {
        Self {
            device_id: hfa_proto::fingerprint(&public_key),
            name: name.into(),
            public_key,
            paired_at: unix_now(),
        }
    }
}

/// On-disk form of the trust store.
#[derive(Serialize, Deserialize)]
struct TrustFile {
    version: u32,
    peers: Vec<TrustedPeer>,
}

/// The set of trusted peers, persisted in `<data_dir>/trusted.json`.
///
/// A cheap-to-clone, thread-safe handle: clones share the same store, and every mutation is
/// saved to disk immediately (atomically: temp file + rename). If saving fails, the in-memory
/// store is left unchanged, so memory and disk never disagree.
#[derive(Clone)]
pub struct TrustStore {
    inner: Arc<parking_lot::Mutex<TrustInner>>,
}

#[derive(Debug)]
struct TrustInner {
    path: PathBuf,
    peers: Vec<TrustedPeer>,
}

impl fmt::Debug for TrustStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let inner = self.inner.lock();
        f.debug_struct("TrustStore")
            .field("path", &inner.path)
            .field("peers", &inner.peers.len())
            .finish()
    }
}

impl TrustStore {
    /// Loads `<data_dir>/trusted.json` (empty store if missing). Entries whose `device_id`
    /// does not match their key are dropped with a warning.
    ///
    /// # Errors
    /// [`crate::CoreError::Io`] / [`crate::CoreError::Json`], [`crate::CoreError::Config`]
    /// for an unsupported file version.
    pub fn load(data_dir: &Path) -> Result<TrustStore> {
        let path = data_dir.join(TRUST_FILE);
        let peers = match std::fs::read(&path) {
            Ok(bytes) => {
                let file: TrustFile = serde_json::from_slice(&bytes)?;
                if file.version != FILE_FORMAT_VERSION {
                    return Err(CoreError::Config(format!(
                        "unsupported trust store version {}",
                        file.version
                    )));
                }
                let mut peers: Vec<TrustedPeer> = Vec::with_capacity(file.peers.len());
                for peer in file.peers {
                    if peer.device_id != hfa_proto::fingerprint(&peer.public_key) {
                        tracing::warn!(device_id = %peer.device_id, "dropping a trusted peer whose id does not match its key");
                    } else if !peers.iter().any(|p| p.public_key == peer.public_key) {
                        peers.push(peer);
                    }
                }
                peers
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        Ok(TrustStore {
            inner: Arc::new(parking_lot::Mutex::new(TrustInner { path, peers })),
        })
    }

    /// `true` if a peer with this public key is trusted.
    pub fn is_trusted(&self, public_key: &[u8; 32]) -> bool {
        self.inner
            .lock()
            .peers
            .iter()
            .any(|p| p.public_key == *public_key)
    }

    /// Looks a peer up by device id.
    pub fn get(&self, device_id: &str) -> Option<TrustedPeer> {
        self.inner
            .lock()
            .peers
            .iter()
            .find(|p| p.device_id == device_id)
            .cloned()
    }

    /// Adds or replaces (same device id) a peer and saves.
    ///
    /// # Errors
    /// [`crate::CoreError::Config`] if `peer.device_id` is not the fingerprint of
    /// `peer.public_key`; [`crate::CoreError::Io`] / [`crate::CoreError::Json`] (the store is
    /// then unchanged).
    pub fn add(&self, peer: TrustedPeer) -> Result<()> {
        if peer.device_id != hfa_proto::fingerprint(&peer.public_key) {
            return Err(CoreError::Config(format!(
                "device id {} does not match the peer's key",
                peer.device_id
            )));
        }
        let mut inner = self.inner.lock();
        let mut peers = inner.peers.clone();
        match peers.iter_mut().find(|p| p.device_id == peer.device_id) {
            Some(existing) => *existing = peer,
            None => peers.push(peer),
        }
        save_peers(&inner.path, &peers)?;
        inner.peers = peers;
        Ok(())
    }

    /// Removes a peer by device id and saves. Returns `true` if it existed.
    ///
    /// # Errors
    /// [`crate::CoreError::Io`] / [`crate::CoreError::Json`] (the store is then unchanged).
    pub fn remove(&self, device_id: &str) -> Result<bool> {
        let mut inner = self.inner.lock();
        let Some(pos) = inner.peers.iter().position(|p| p.device_id == device_id) else {
            return Ok(false);
        };
        let mut peers = inner.peers.clone();
        peers.remove(pos);
        save_peers(&inner.path, &peers)?;
        inner.peers = peers;
        Ok(true)
    }

    /// All trusted peers.
    pub fn peers(&self) -> Vec<TrustedPeer> {
        self.inner.lock().peers.clone()
    }
}

fn save_peers(path: &Path, peers: &[TrustedPeer]) -> Result<()> {
    #[derive(Serialize)]
    struct TrustFileRef<'a> {
        version: u32,
        peers: &'a [TrustedPeer],
    }
    let json = serde_json::to_vec_pretty(&TrustFileRef {
        version: FILE_FORMAT_VERSION,
        peers,
    })?;
    write_file_atomic(path, &json, true)
}

/// Current unix time in seconds (0 if the clock is before 1970).
pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_file_rejects_bad_keys() {
        let ok = |private: &str, public: &str| {
            parse_identity_file(IdentityFile {
                version: 1,
                private_key: private.into(),
                public_key: public.into(),
            })
        };
        let k = B64.encode([5u8; 32]);
        assert!(ok(&k, &k).is_ok());
        assert!(matches!(ok("!!", &k), Err(CoreError::Config(_))));
        assert!(matches!(
            ok(&B64.encode([5u8; 31]), &k),
            Err(CoreError::Config(_))
        ));
        assert!(matches!(
            ok(&k, &B64.encode([0u8; 32])),
            Err(CoreError::Config(_))
        ));
        let bad_version = parse_identity_file(IdentityFile {
            version: 9,
            private_key: k.clone(),
            public_key: k,
        });
        assert!(matches!(bad_version, Err(CoreError::Config(_))));
    }

    #[test]
    fn trusted_peer_json_uses_base64_keys() {
        let peer = TrustedPeer::new([9u8; 32], "Desk");
        let json = serde_json::to_string(&peer).expect("json");
        assert!(json.contains(&B64.encode([9u8; 32])), "{json}");
        let back: TrustedPeer = serde_json::from_str(&json).expect("parse");
        assert_eq!(back, peer);
    }
}
