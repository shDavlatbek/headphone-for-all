//! Device identity (Noise static keypair) and the store of trusted (paired) peers.
//!
//! Files in the data directory:
//! - [`IDENTITY_FILE`] (`identity.json`): `{"version":1,"private_key":"<b64>","public_key":"<b64>"}`
//!   (standard base64 with padding). On unix it is created with mode `0600` directly (the
//!   temporary file is created with that mode, so the private key is never readable by other
//!   users, not even briefly), and an existing file with a wider mode is tightened on load.
//!   The public key must be the one derived from the private key (checked on load).
//! - [`TRUST_FILE`] (`trusted.json`): `{"version":1,"peers":[{"device_id":..,"name":..,
//!   "public_key":"<b64>","paired_at":<unix s>,"roles":{"hub":..,"sender":..}}]}`,
//!   rewritten atomically (temp file + rename) on every change, mode `0600` on unix. Writers
//!   of every process serialize on an advisory lock of [`TRUST_LOCK_FILE`] and re-read the
//!   file before changing it (see [`TrustStore`]). `roles` ([`PeerRoles`]) says in which
//!   direction a peer was paired; entries without it (older files) are trusted both ways.
//!
//! The `device_id` is always [`hfa_proto::fingerprint`] of the static public key.

use std::collections::HashMap;
use std::fmt;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime};

use base64::Engine as _;
use hfa_proto::StaticKeypair;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
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
    // The Noise handshake uses the key derived from the private key, while the device id
    // (and the hub's pairing URI / mDNS id) comes from the stored public key: a mismatch
    // would load fine and then fail every connection with a confusing key error.
    let keypair = StaticKeypair::from_private(&private)?;
    if keypair.public != public {
        return Err(CoreError::Config(
            "identity file: public key does not match private key".into(),
        ));
    }
    Ok(keypair)
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

/// A direction in which a peer was paired with this device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PeerRole {
    /// The peer is a hub: this device paired with it as a sender and may stream to it.
    Hub,
    /// The peer is a sender: it paired with this device's hub and may stream into it.
    Sender,
}

/// The directions a trusted peer was paired in (`"roles":{"hub":..,"sender":..}` in
/// `trusted.json`).
///
/// Pairing grants trust in one direction only: a sender that paired with a hub trusts it
/// *as a hub*, and the hub trusts the sender *as a sender*. So pairing this device as a
/// sender with a friend's hub never lets that hub stream into this device's own hub
/// without a pairing of its own. Entries written before roles existed have no `roles`
/// field and load as [`PeerRoles::BOTH`] (the behaviour they were created with).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PeerRoles {
    /// Trusted as a hub (see [`PeerRole::Hub`]).
    pub hub: bool,
    /// Trusted as a sender (see [`PeerRole::Sender`]).
    pub sender: bool,
}

impl PeerRoles {
    /// Trusted in both directions (entries from before roles existed, manual trust).
    pub const BOTH: PeerRoles = PeerRoles {
        hub: true,
        sender: true,
    };

    /// Only the given role.
    pub const fn only(role: PeerRole) -> PeerRoles {
        match role {
            PeerRole::Hub => PeerRoles {
                hub: true,
                sender: false,
            },
            PeerRole::Sender => PeerRoles {
                hub: false,
                sender: true,
            },
        }
    }

    /// `true` if `role` is granted.
    pub const fn contains(self, role: PeerRole) -> bool {
        match role {
            PeerRole::Hub => self.hub,
            PeerRole::Sender => self.sender,
        }
    }

    /// The roles granted by either `self` or `other`.
    #[must_use]
    pub const fn union(self, other: PeerRoles) -> PeerRoles {
        PeerRoles {
            hub: self.hub || other.hub,
            sender: self.sender || other.sender,
        }
    }
}

impl Default for PeerRoles {
    /// [`PeerRoles::BOTH`]: what an entry without a `roles` field (written before roles
    /// existed) was trusted for.
    fn default() -> Self {
        PeerRoles::BOTH
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
    /// The directions this peer is trusted in (missing in older files: both).
    #[serde(default)]
    pub roles: PeerRoles,
}

impl TrustedPeer {
    /// A peer trusted in **both** directions ([`PeerRoles::BOTH`]), with `device_id` derived
    /// from `public_key` and `paired_at` = now. Pairings record one direction only: use
    /// [`TrustedPeer::paired_as`] for them.
    pub fn new(public_key: [u8; 32], name: impl Into<String>) -> Self {
        Self {
            device_id: hfa_proto::fingerprint(&public_key),
            name: name.into(),
            public_key,
            paired_at: unix_now(),
            roles: PeerRoles::BOTH,
        }
    }

    /// A peer trusted only as `role` (what a pairing records), otherwise like
    /// [`TrustedPeer::new`].
    pub fn paired_as(public_key: [u8; 32], name: impl Into<String>, role: PeerRole) -> Self {
        Self {
            roles: PeerRoles::only(role),
            ..Self::new(public_key, name)
        }
    }
}

/// On-disk form of the trust store.
#[derive(Serialize, Deserialize)]
struct TrustFile {
    version: u32,
    peers: Vec<TrustedPeer>,
}

/// File name of the lock file that serializes trust-store writers of all processes.
pub const TRUST_LOCK_FILE: &str = "trusted.json.lock";
/// How often a reader checks whether another process changed `trusted.json`.
pub const TRUST_RELOAD_INTERVAL: Duration = Duration::from_secs(1);

/// The set of trusted peers, persisted in `<data_dir>/trusted.json`.
///
/// **One store per data directory and process:** [`TrustStore::load`] returns a handle to
/// the same shared store for the same directory as long as any handle to it is alive, so
/// the hub, the sender and the app's settings see each other's changes at once, and
/// [`TrustStore::subscribe`] tells them about every change. Handles are cheap to clone and
/// thread-safe.
///
/// **The file is the source of truth:** [`TrustStore::add`] and [`TrustStore::remove`] take an
/// exclusive advisory lock on `<data_dir>/`[`TRUST_LOCK_FILE`] (other processes: CLI, a
/// second app instance, the iOS extension), re-read `trusted.json`, apply their change to
/// what is on disk and write it back atomically (temp file + rename). So no update of another
/// handle or process is lost, and a peer removed elsewhere never comes back with the next
/// save. If saving fails, the in-memory store is left unchanged.
///
/// Readers ([`TrustStore::is_trusted`], [`TrustStore::is_trusted_as`], [`TrustStore::get`],
/// [`TrustStore::peers`]) never wait for a writer, so they may be called from async tasks:
/// they only copy the in-memory list, and at most once per [`TRUST_RELOAD_INTERVAL`] one of
/// them checks the file's metadata and re-reads the file if another process changed it
/// (skipped while a writer of this process is busy; a file that cannot be read keeps the
/// current list). [`TrustStore::add`], [`TrustStore::remove`] and [`TrustStore::reload`]
/// block (file lock, write + fsync); call them from `spawn_blocking` or a non-async thread.
#[derive(Clone)]
pub struct TrustStore {
    inner: Arc<TrustInner>,
}

struct TrustInner {
    path: PathBuf,
    lock_path: PathBuf,
    /// The current peer list and what it was read from. Held only for short, I/O-free
    /// sections.
    state: parking_lot::Mutex<TrustState>,
    /// Serializes this process's writers (and forced reloads) for the whole
    /// read-modify-save-swap sequence.
    save_lock: parking_lot::Mutex<()>,
    /// Change counter, bumped whenever the peer list changes.
    changes: watch::Sender<u64>,
}

struct TrustState {
    peers: Vec<TrustedPeer>,
    /// Identity of the `trusted.json` version `peers` reflects (`None`: no file).
    stamp: Option<FileStamp>,
    /// When a reader last compared `stamp` with the file.
    checked: Instant,
}

/// What identifies one version of `trusted.json` (every save replaces the file).
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    inode: (u64, u64),
}

impl FileStamp {
    fn of(meta: &std::fs::Metadata) -> FileStamp {
        FileStamp {
            len: meta.len(),
            modified: meta.modified().ok(),
            #[cfg(unix)]
            inode: {
                use std::os::unix::fs::MetadataExt;
                (meta.dev(), meta.ino())
            },
        }
    }
}

/// Every live store of this process, by canonical trust-file path.
fn registry() -> &'static parking_lot::Mutex<HashMap<PathBuf, Weak<TrustInner>>> {
    static REGISTRY: OnceLock<parking_lot::Mutex<HashMap<PathBuf, Weak<TrustInner>>>> =
        OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

impl fmt::Debug for TrustStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TrustStore")
            .field("path", &self.inner.path)
            .field("peers", &self.inner.state.lock().peers.len())
            .finish()
    }
}

impl TrustStore {
    /// Opens the trust store of `data_dir` (empty if `trusted.json` is missing). If this
    /// process already has a live store for that directory, returns a handle to it (after
    /// picking up changes on disk). Entries whose `device_id` does not match their key are
    /// dropped with a warning.
    ///
    /// # Errors
    /// [`crate::CoreError::Io`] / [`crate::CoreError::Json`], [`crate::CoreError::Config`]
    /// for an unsupported file version.
    pub fn load(data_dir: &Path) -> Result<TrustStore> {
        let path = canonical_dir(data_dir).join(TRUST_FILE);
        let mut stores = registry().lock();
        stores.retain(|_, store| store.strong_count() > 0);
        if let Some(inner) = stores.get(&path).and_then(Weak::upgrade) {
            drop(stores);
            let store = TrustStore { inner };
            store.reload()?;
            return Ok(store);
        }
        let (peers, stamp) = read_trust_file(&path)?;
        let inner = Arc::new(TrustInner {
            lock_path: path.with_file_name(TRUST_LOCK_FILE),
            path: path.clone(),
            state: parking_lot::Mutex::new(TrustState {
                peers,
                stamp,
                checked: Instant::now(),
            }),
            save_lock: parking_lot::Mutex::new(()),
            changes: watch::Sender::new(0),
        });
        stores.insert(path, Arc::downgrade(&inner));
        Ok(TrustStore { inner })
    }

    /// `true` if a peer with this public key is trusted (in any role).
    pub fn is_trusted(&self, public_key: &[u8; 32]) -> bool {
        self.read(|peers| peers.iter().any(|p| p.public_key == *public_key))
    }

    /// `true` if a peer with this public key is trusted as `role`.
    pub fn is_trusted_as(&self, public_key: &[u8; 32], role: PeerRole) -> bool {
        self.read(|peers| {
            peers
                .iter()
                .any(|p| p.public_key == *public_key && p.roles.contains(role))
        })
    }

    /// Looks a peer up by device id.
    pub fn get(&self, device_id: &str) -> Option<TrustedPeer> {
        self.read(|peers| peers.iter().find(|p| p.device_id == device_id).cloned())
    }

    /// All trusted peers.
    pub fn peers(&self) -> Vec<TrustedPeer> {
        self.read(<[TrustedPeer]>::to_vec)
    }

    /// A receiver of the store's change counter: it changes whenever the peer list does
    /// (a pairing, a removal, a change by another process picked up by a reader or
    /// [`TrustStore::reload`]). The hub uses it to disconnect senders that were removed.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.inner.changes.subscribe()
    }

    /// Re-reads `trusted.json` now if it changed on disk. Returns `true` if the peer list
    /// changed. Blocks (waits for this process's writers).
    ///
    /// # Errors
    /// As [`TrustStore::load`] (the in-memory list is then unchanged).
    pub fn reload(&self) -> Result<bool> {
        let _writer = self.inner.save_lock.lock();
        self.refresh_locked()
    }

    /// Adds a peer and saves. A peer with the same device id is replaced, keeping the roles
    /// it already had ([`PeerRoles::union`]), so pairing in the other direction adds a role.
    ///
    /// # Errors
    /// [`crate::CoreError::Config`] if `peer.device_id` is not the fingerprint of
    /// `peer.public_key`; [`crate::CoreError::Io`] / [`crate::CoreError::Json`] (the store is
    /// then unchanged).
    pub fn add(&self, mut peer: TrustedPeer) -> Result<()> {
        if peer.device_id != hfa_proto::fingerprint(&peer.public_key) {
            return Err(CoreError::Config(format!(
                "device id {} does not match the peer's key",
                peer.device_id
            )));
        }
        self.modify(move |peers| {
            match peers.iter_mut().find(|p| p.device_id == peer.device_id) {
                Some(existing) => {
                    peer.roles = peer.roles.union(existing.roles);
                    *existing = peer;
                }
                None => peers.push(peer),
            }
            (true, ())
        })
    }

    /// Removes a peer by device id and saves. Returns `true` if it existed.
    ///
    /// # Errors
    /// [`crate::CoreError::Io`] / [`crate::CoreError::Json`] (the store is then unchanged).
    pub fn remove(&self, device_id: &str) -> Result<bool> {
        self.modify(
            |peers| match peers.iter().position(|p| p.device_id == device_id) {
                Some(pos) => {
                    peers.remove(pos);
                    (true, true)
                }
                None => (false, false),
            },
        )
    }

    /// Runs `f` on the in-memory list, first picking up another process's change of the
    /// file if one is due (never waits for a writer).
    fn read<R>(&self, f: impl FnOnce(&[TrustedPeer]) -> R) -> R {
        let due = self.inner.state.lock().checked.elapsed() >= TRUST_RELOAD_INTERVAL;
        if due {
            if let Some(_writer) = self.inner.save_lock.try_lock() {
                if let Err(e) = self.refresh_locked() {
                    tracing::warn!(path = %self.inner.path.display(), error = %e, "cannot re-read the trust store; keeping the loaded one");
                }
            }
        }
        f(&self.inner.state.lock().peers)
    }

    /// Re-reads the file if its stamp changed. The caller holds `save_lock`.
    fn refresh_locked(&self) -> Result<bool> {
        let stamp = file_stamp(&self.inner.path)?;
        {
            let mut state = self.inner.state.lock();
            state.checked = Instant::now();
            if state.stamp == stamp {
                return Ok(false);
            }
        }
        let (peers, stamp) = read_trust_file(&self.inner.path)?;
        Ok(self.swap(peers, stamp))
    }

    /// Read-modify-write against the file under both locks. `f` returns whether it changed
    /// the list (then it is saved) and the result.
    fn modify<R>(&self, f: impl FnOnce(&mut Vec<TrustedPeer>) -> (bool, R)) -> Result<R> {
        let _writer = self.inner.save_lock.lock();
        let _file_lock = lock_file(&self.inner.lock_path)?;
        let (mut peers, mut stamp) = read_trust_file(&self.inner.path)?;
        let (changed, out) = f(&mut peers);
        if changed {
            save_peers(&self.inner.path, &peers)?;
            stamp = file_stamp(&self.inner.path)?;
        }
        self.swap(peers, stamp);
        Ok(out)
    }

    /// Installs a list read from or written to disk; notifies subscribers if it differs.
    fn swap(&self, peers: Vec<TrustedPeer>, stamp: Option<FileStamp>) -> bool {
        let changed = {
            let mut state = self.inner.state.lock();
            state.stamp = stamp;
            state.checked = Instant::now();
            if state.peers == peers {
                false
            } else {
                state.peers = peers;
                true
            }
        };
        if changed {
            self.inner.changes.send_modify(|n| *n = n.wrapping_add(1));
        }
        changed
    }
}

/// `dir` with symlinks resolved (for the part of it that exists), so every spelling of a
/// data directory maps to the same store.
fn canonical_dir(dir: &Path) -> PathBuf {
    let absolute = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut existing = absolute.as_path();
    let mut rest = Vec::new();
    loop {
        if let Ok(real) = std::fs::canonicalize(existing) {
            return rest.iter().rev().fold(real, |p, c| p.join(c));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                existing = parent;
            }
            _ => return absolute,
        }
    }
}

/// The stamp of the file at `path` (`None` if it does not exist).
fn file_stamp(path: &Path) -> Result<Option<FileStamp>> {
    match std::fs::metadata(path) {
        Ok(meta) => Ok(Some(FileStamp::of(&meta))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Reads and validates `trusted.json` (empty if missing), with the stamp of the version read.
fn read_trust_file(path: &Path) -> Result<(Vec<TrustedPeer>, Option<FileStamp>)> {
    let mut file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), None)),
        Err(e) => return Err(e.into()),
    };
    // The stamp of the open file is the stamp of exactly the version read.
    let stamp = FileStamp::of(&file.metadata()?);
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
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
    Ok((peers, Some(stamp)))
}

/// Opens (creating it and its directory if needed) and exclusively locks the trust-store
/// lock file; the lock is released when the returned file is dropped.
fn lock_file(path: &Path) -> Result<std::fs::File> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    // Fully qualified: std's inherent `File::lock` needs a newer Rust than the MSRV.
    fs4::FileExt::lock(&file)?;
    Ok(file)
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
        let kp = StaticKeypair::generate().expect("keypair");
        let k = B64.encode(kp.private);
        let public = B64.encode(kp.public);
        assert_eq!(ok(&k, &public).expect("valid"), kp);
        assert!(matches!(ok("!!", &public), Err(CoreError::Config(_))));
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
            public_key: public,
        });
        assert!(matches!(bad_version, Err(CoreError::Config(_))));
        // A valid private key next to a public key that is not its own.
        assert!(matches!(
            ok(&k, &B64.encode([9u8; 32])),
            Err(CoreError::Config(e)) if e.contains("does not match")
        ));
    }

    #[test]
    fn identity_with_a_mismatched_public_key_is_rejected_on_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let created = Identity::load_or_create(dir.path(), "Desk").expect("create");
        let path = dir.path().join(IDENTITY_FILE);
        let text = std::fs::read_to_string(&path).expect("read");
        let edited = text.replace(&B64.encode(created.keypair.public), &B64.encode([9u8; 32]));
        assert_ne!(text, edited);
        std::fs::write(&path, edited).expect("write");
        assert!(matches!(
            Identity::load_or_create(dir.path(), "Desk"),
            Err(CoreError::Config(_))
        ));
    }

    #[test]
    fn readers_never_wait_for_a_save() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TrustStore::load(dir.path()).expect("load");
        store.add(TrustedPeer::new([1u8; 32], "A")).expect("add");
        // Simulate a writer in the middle of its (slow) save.
        let writer = store.inner.save_lock.lock();
        let reader = store.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let seen = (
                reader.is_trusted(&[1u8; 32]),
                reader.get(&hfa_proto::fingerprint(&[1u8; 32])).is_some(),
                reader.peers().len(),
            );
            let _ = tx.send(seen);
        });
        let seen = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("readers must not block on the save lock");
        assert_eq!(seen, (true, true, 1));
        drop(writer);
        handle.join().expect("reader thread");
    }

    #[test]
    fn concurrent_writers_lose_no_update() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TrustStore::load(dir.path()).expect("load");
        let handles: Vec<_> = (0..8u8)
            .map(|i| {
                let store = store.clone();
                std::thread::spawn(move || {
                    store
                        .add(TrustedPeer::new([i + 1; 32], format!("P{i}")))
                        .expect("add")
                })
            })
            .collect();
        for h in handles {
            h.join().expect("writer");
        }
        assert_eq!(store.peers().len(), 8);
        assert_eq!(
            TrustStore::load(dir.path()).expect("reload").peers().len(),
            8
        );
    }

    #[test]
    fn trusted_peer_json_uses_base64_keys() {
        let peer = TrustedPeer::new([9u8; 32], "Desk");
        let json = serde_json::to_string(&peer).expect("json");
        assert!(json.contains(&B64.encode([9u8; 32])), "{json}");
        let back: TrustedPeer = serde_json::from_str(&json).expect("parse");
        assert_eq!(back, peer);
    }

    #[test]
    fn stores_of_one_directory_are_shared_in_a_process() {
        let dir = tempfile::tempdir().expect("tempdir");
        // An engine loaded the store; the FFI/CLI loads it again and forgets a peer.
        let engine = TrustStore::load(dir.path()).expect("load");
        engine.add(TrustedPeer::new([1u8; 32], "A")).expect("add");
        let ui = TrustStore::load(dir.path()).expect("load again");
        assert!(ui.is_trusted(&[1u8; 32]));
        assert!(ui
            .remove(&hfa_proto::fingerprint(&[1u8; 32]))
            .expect("remove"));
        assert!(
            !engine.is_trusted(&[1u8; 32]),
            "the running engine sees the removal"
        );
        // The same directory spelled differently is the same store.
        let other_spelling = TrustStore::load(&dir.path().join(".")).expect("load");
        other_spelling
            .add(TrustedPeer::new([2u8; 32], "B"))
            .expect("add");
        assert!(engine.is_trusted(&[2u8; 32]));
    }

    #[test]
    fn writers_merge_with_changes_saved_by_another_process() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = TrustStore::load(dir.path()).expect("load");
        store.add(TrustedPeer::new([1u8; 32], "A")).expect("add");
        store.add(TrustedPeer::new([2u8; 32], "B")).expect("add");
        // Another process (e.g. `hfa trust remove`) removes A from the file.
        let b = store.get(&hfa_proto::fingerprint(&[2u8; 32])).expect("B");
        save_peers(&dir.path().join(TRUST_FILE), &[b]).expect("external save");
        // A pairing in this process must not bring A back.
        store.add(TrustedPeer::new([3u8; 32], "C")).expect("add");
        assert!(!store.is_trusted(&[1u8; 32]));
        assert!(store.is_trusted(&[2u8; 32]) && store.is_trusted(&[3u8; 32]));
        let (on_disk, _) = read_trust_file(&dir.path().join(TRUST_FILE)).expect("read");
        assert_eq!(on_disk.len(), 2);
        // reload() picks up an external change without a write.
        save_peers(&dir.path().join(TRUST_FILE), &[]).expect("external save");
        store.reload().expect("reload");
        assert!(store.peers().is_empty());
    }
}
