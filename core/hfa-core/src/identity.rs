//! Device identity (Noise static keypair) and the store of trusted (paired) peers.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use hfa_proto::StaticKeypair;
use serde::{Deserialize, Serialize};

use crate::Result;

/// File name of the identity inside the data directory (unix mode 0600).
pub const IDENTITY_FILE: &str = "identity.json";
/// File name of the trust store inside the data directory.
pub const TRUST_FILE: &str = "trusted.json";

/// This device's long-term identity.
#[derive(Debug, Clone)]
pub struct Identity {
    /// Noise static keypair.
    pub keypair: StaticKeypair,
    /// [`hfa_proto::fingerprint`] of the public key.
    pub device_id: String,
    /// Display name.
    pub name: String,
}

impl Identity {
    /// Loads `<data_dir>/identity.json`, or creates a new keypair and saves it (unix mode
    /// 0600). `name` becomes the display name (it is not persisted in the identity file).
    ///
    /// # Errors
    /// [`crate::CoreError::Io`] / [`crate::CoreError::Json`] / [`crate::CoreError::Proto`].
    pub fn load_or_create(_data_dir: &Path, _name: &str) -> Result<Identity> {
        todo!("feat/core-engine")
    }

    /// The static public key.
    pub fn public_key(&self) -> [u8; 32] {
        self.keypair.public
    }
}

/// A paired peer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedPeer {
    /// Fingerprint of `public_key`.
    pub device_id: String,
    /// Display name at pairing time.
    pub name: String,
    /// Pinned Noise static public key.
    pub public_key: [u8; 32],
    /// Unix time (seconds) of pairing.
    pub paired_at: u64,
}

/// The set of trusted peers, persisted in `<data_dir>/trusted.json`.
///
/// A cheap-to-clone, thread-safe handle: clones share the same store, and every mutation is
/// saved to disk immediately.
#[derive(Debug, Clone)]
pub struct TrustStore {
    inner: Arc<parking_lot::Mutex<TrustInner>>,
}

#[derive(Debug)]
struct TrustInner {
    path: PathBuf,
    peers: Vec<TrustedPeer>,
}

impl TrustStore {
    /// Loads `<data_dir>/trusted.json` (empty store if missing).
    ///
    /// # Errors
    /// [`crate::CoreError::Io`] / [`crate::CoreError::Json`].
    pub fn load(_data_dir: &Path) -> Result<TrustStore> {
        todo!("feat/core-engine")
    }

    /// `true` if a peer with this public key is trusted.
    pub fn is_trusted(&self, _public_key: &[u8; 32]) -> bool {
        let _ = &self.inner.lock().path;
        todo!("feat/core-engine")
    }

    /// Looks a peer up by device id.
    pub fn get(&self, _device_id: &str) -> Option<TrustedPeer> {
        let _ = &self.inner.lock().peers;
        todo!("feat/core-engine")
    }

    /// Adds or replaces (same device id) a peer and saves.
    ///
    /// # Errors
    /// [`crate::CoreError::Io`] / [`crate::CoreError::Json`].
    pub fn add(&self, _peer: TrustedPeer) -> Result<()> {
        todo!("feat/core-engine")
    }

    /// Removes a peer by device id and saves. Returns `true` if it existed.
    ///
    /// # Errors
    /// [`crate::CoreError::Io`] / [`crate::CoreError::Json`].
    pub fn remove(&self, _device_id: &str) -> Result<bool> {
        todo!("feat/core-engine")
    }

    /// All trusted peers.
    pub fn peers(&self) -> Vec<TrustedPeer> {
        todo!("feat/core-engine")
    }
}
