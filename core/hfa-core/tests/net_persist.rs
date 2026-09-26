//! Settings, identity and trust store persistence (tempdir).

use hfa_capture::OutputTarget;
use hfa_core::config::{Settings, SETTINGS_FILE};
use hfa_core::identity::{IDENTITY_FILE, TRUST_FILE, TRUST_RELOAD_INTERVAL};
use hfa_core::{CoreError, Identity, PeerRole, PeerRoles, TrustStore, TrustedPeer};

/// No temporary files are left behind by atomic writes.
fn assert_no_temp_files(dir: &std::path::Path) {
    for entry in std::fs::read_dir(dir).expect("read_dir") {
        let name = entry.expect("entry").file_name();
        let name = name.to_string_lossy();
        assert!(!name.ends_with(".tmp"), "leftover temp file {name}");
    }
}

#[test]
fn settings_default_save_and_reload() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("nested").join("hfa");
    let s = Settings::load_or_default(&dir).expect("defaults");
    assert!(dir.is_dir(), "load_or_default creates the directory");
    assert_eq!(s.data_dir, dir);
    assert_eq!(s.port, 47810);
    assert!(
        !dir.join(SETTINGS_FILE).exists(),
        "defaults are not written implicitly"
    );

    let mut changed = s.clone();
    changed.device_name = "Wohnzimmer 🎧".into();
    changed.port = 0;
    changed.bitrate = 96_000;
    changed.frame_ms = 20;
    changed.fec = false;
    changed.jitter_min_ms = 30;
    changed.jitter_max_ms = 30;
    changed.output = OutputTarget::Null;
    changed.save().expect("save");
    changed.save().expect("save again (replaces atomically)");
    assert_no_temp_files(&dir);

    let back = Settings::load_or_default(&dir).expect("reload");
    assert_eq!(back, changed);
}

#[test]
fn invalid_settings_are_refused() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = Settings::load_or_default(tmp.path()).expect("defaults");
    let cases: [fn(&mut Settings); 10] = [
        |s| s.frame_ms = 15,
        |s| s.frame_ms = 5,
        |s| s.bitrate = 5_999,
        |s| s.bitrate = 510_001,
        |s| {
            s.jitter_min_ms = 100;
            s.jitter_max_ms = 50;
        },
        |s| s.jitter_min_ms = 0,
        |s| s.jitter_max_ms = 60_000,
        |s| s.device_name = "  ".into(),
        |s| s.device_name = "a\nb".into(),
        |s| s.device_name = "x".repeat(300),
    ];
    for (i, mutate) in cases.iter().enumerate() {
        let mut s = base.clone();
        mutate(&mut s);
        assert!(
            matches!(s.validate(), Err(CoreError::Config(_))),
            "case {i} should be invalid"
        );
        assert!(matches!(s.save(), Err(CoreError::Config(_))), "case {i}");
        assert!(!tmp.path().join(SETTINGS_FILE).exists());
    }
    base.validate().expect("defaults are valid");

    // An invalid value in the file is reported, not silently replaced.
    std::fs::write(tmp.path().join(SETTINGS_FILE), r#"{"frame_ms": 15}"#).expect("write");
    assert!(matches!(
        Settings::load_or_default(tmp.path()),
        Err(CoreError::Config(_))
    ));
    std::fs::write(tmp.path().join(SETTINGS_FILE), "{not json").expect("write");
    assert!(matches!(
        Settings::load_or_default(tmp.path()),
        Err(CoreError::Json(_))
    ));
}

#[test]
fn identity_is_created_once_and_reloaded() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("data");
    let a = Identity::load_or_create(&dir, "Desk").expect("create");
    assert_eq!(a.device_id, hfa_proto::fingerprint(&a.public_key()));
    assert!(hfa_proto::is_fingerprint(&a.device_id));
    assert_eq!(a.name, "Desk");
    let b = Identity::load_or_create(&dir, "Renamed").expect("load");
    assert_eq!(b.keypair, a.keypair, "same key after reload");
    assert_eq!(b.device_id, a.device_id);
    assert_eq!(
        b.name, "Renamed",
        "the name is not persisted in the identity"
    );
    assert_no_temp_files(&dir);

    // The keys are stored as base64 and never printed by Debug.
    let text = std::fs::read_to_string(dir.join(IDENTITY_FILE)).expect("read");
    let json: serde_json::Value = serde_json::from_str(&text).expect("json");
    assert_eq!(json["version"], 1);
    assert!(json["private_key"].is_string() && json["public_key"].is_string());
    let debug = format!("{a:?}");
    let private_b64 = json["private_key"].as_str().expect("str");
    assert!(!debug.contains(private_b64));

    // A corrupt identity is an error, never silently replaced.
    std::fs::write(dir.join(IDENTITY_FILE), "{}").expect("write");
    assert!(Identity::load_or_create(&dir, "Desk").is_err());
}

#[cfg(unix)]
#[test]
fn identity_file_is_private() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().expect("tempdir");
    Identity::load_or_create(tmp.path(), "Desk").expect("create");
    let path = tmp.path().join(IDENTITY_FILE);
    let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);

    // A file that became world-readable is tightened on load.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
    Identity::load_or_create(tmp.path(), "Desk").expect("load");
    let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);

    // The trust store is private too.
    let trust = TrustStore::load(tmp.path()).expect("trust");
    trust.add(TrustedPeer::new([1; 32], "Peer")).expect("add");
    let mode = std::fs::metadata(tmp.path().join(TRUST_FILE))
        .expect("meta")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn concurrent_identity_creation_agrees_on_one_key() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().to_path_buf();
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let dir = dir.clone();
            std::thread::spawn(move || Identity::load_or_create(&dir, "Desk").expect("identity"))
        })
        .collect();
    let ids: Vec<String> = handles
        .into_iter()
        .map(|h| h.join().expect("join").device_id)
        .collect();
    assert!(ids.windows(2).all(|w| w[0] == w[1]), "{ids:?}");
    assert_no_temp_files(&dir);
}

#[test]
fn trust_store_is_shared_and_persisted() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = TrustStore::load(tmp.path()).expect("empty");
    assert!(store.peers().is_empty());
    let clone = store.clone();

    let a = TrustedPeer::new([1; 32], "Laptop");
    let b = TrustedPeer::new([2; 32], "Phone");
    store.add(a.clone()).expect("add a");
    clone.add(b.clone()).expect("add b");
    assert!(store.is_trusted(&[2; 32]) && clone.is_trusted(&[1; 32]));
    assert!(!store.is_trusted(&[3; 32]));
    assert_eq!(store.get(&a.device_id), Some(a.clone()));

    // Re-adding the same device replaces it (e.g. new name).
    let mut renamed = a.clone();
    renamed.name = "Work laptop".into();
    store.add(renamed.clone()).expect("replace");
    assert_eq!(store.peers().len(), 2);

    let reloaded = TrustStore::load(tmp.path()).expect("reload");
    assert_eq!(reloaded.get(&a.device_id), Some(renamed));
    assert_eq!(reloaded.get(&b.device_id), Some(b.clone()));

    assert!(store.remove(&b.device_id).expect("remove"));
    assert!(!store.remove(&b.device_id).expect("already gone"));
    assert!(!clone.is_trusted(&[2; 32]));
    assert!(TrustStore::load(tmp.path())
        .expect("reload")
        .get(&b.device_id)
        .is_none());
    assert_no_temp_files(tmp.path());

    // A peer whose id does not match its key is refused.
    let mut forged = TrustedPeer::new([4; 32], "Forged");
    forged.device_id = a.device_id.clone();
    assert!(matches!(store.add(forged), Err(CoreError::Config(_))));
}

#[test]
fn trust_store_drops_tampered_entries_on_load() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let good = TrustedPeer::new([1; 32], "Good");
    let key = serde_json::to_value(&good).expect("json")["public_key"].clone();
    let json = serde_json::json!({
        "version": 1,
        "peers": [
            good,
            {"device_id": "0000-0000-0000-0000", "name": "Bad", "public_key": key, "paired_at": 1}
        ]
    });
    std::fs::write(tmp.path().join(TRUST_FILE), json.to_string()).expect("write");
    let store = TrustStore::load(tmp.path()).expect("load");
    assert_eq!(store.peers(), vec![good]);

    std::fs::write(
        tmp.path().join(TRUST_FILE),
        r#"{"version": 7, "peers": []}"#,
    )
    .expect("w");
    assert!(matches!(
        TrustStore::load(tmp.path()),
        Err(CoreError::Config(_))
    ));
}

/// Writes `trusted.json` the way another process would (atomically, new inode).
fn write_trust_file_externally(dir: &std::path::Path, peers: &[TrustedPeer]) {
    let json = serde_json::json!({ "version": 1, "peers": peers });
    let tmp = dir.join("external.json.part");
    std::fs::write(&tmp, json.to_string()).expect("write");
    std::fs::rename(&tmp, dir.join(TRUST_FILE)).expect("rename");
}

fn ids(store: &TrustStore) -> Vec<String> {
    let mut ids: Vec<String> = store.peers().into_iter().map(|p| p.device_id).collect();
    ids.sort();
    ids
}

#[test]
fn separately_loaded_stores_of_one_dir_share_changes() {
    // Regression: every `load` used to create an independent copy, so the hub engine, the
    // sender engine and the app's settings overwrote each other's pairings, and a peer
    // forgotten through one copy came back with the next pairing saved through another.
    let tmp = tempfile::tempdir().expect("tempdir");
    let hub_store = TrustStore::load(tmp.path()).expect("hub");
    // Another spelling of the same directory still reaches the same store.
    let sender_store = TrustStore::load(&tmp.path().join(".")).expect("sender");
    let mut changes = hub_store.subscribe();

    let x = TrustedPeer::paired_as([1; 32], "X", PeerRole::Hub);
    let y = TrustedPeer::paired_as([2; 32], "Y", PeerRole::Sender);
    sender_store.add(x.clone()).expect("sender pairs with X");
    hub_store.add(y.clone()).expect("hub pairs with Y");
    assert!(changes.has_changed().expect("sender alive"));
    changes.mark_unchanged();
    let on_disk = TrustStore::load(tmp.path()).expect("reload");
    assert_eq!(ids(&on_disk), {
        let mut v = vec![x.device_id.clone(), y.device_id.clone()];
        v.sort();
        v
    });

    // The app forgets Y through a third handle: the running engines see it at once.
    let settings_store = TrustStore::load(tmp.path()).expect("settings");
    assert!(settings_store.remove(&y.device_id).expect("forget"));
    assert!(changes.has_changed().expect("sender alive"));
    assert!(!hub_store.is_trusted(&[2; 32]));
    assert!(!sender_store.is_trusted(&[2; 32]));

    // The next pairing does not bring Y back.
    hub_store
        .add(TrustedPeer::paired_as([3; 32], "Z", PeerRole::Sender))
        .expect("pair Z");
    drop((hub_store, sender_store, settings_store, on_disk));
    let fresh = TrustStore::load(tmp.path()).expect("fresh");
    assert!(
        fresh.get(&y.device_id).is_none(),
        "forgotten peer resurrected"
    );
    assert_eq!(fresh.peers().len(), 2);
}

#[test]
fn writers_merge_with_changes_made_by_another_process() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = TrustStore::load(tmp.path()).expect("load");
    let a = TrustedPeer::new([1; 32], "A");
    let b = TrustedPeer::new([2; 32], "B");
    store.add(a.clone()).expect("add a");
    store.add(b.clone()).expect("add b");

    // Another process (e.g. `hfa trust remove`) removes A and adds C.
    let c = TrustedPeer::new([3; 32], "C");
    write_trust_file_externally(tmp.path(), &[b.clone(), c.clone()]);

    // A save through the stale in-memory copy keeps both changes.
    let d = TrustedPeer::new([4; 32], "D");
    store.add(d.clone()).expect("add d");
    let mut expected = vec![b.device_id, c.device_id, d.device_id];
    expected.sort();
    assert_eq!(ids(&store), expected);
    drop(store);
    assert_eq!(
        ids(&TrustStore::load(tmp.path()).expect("reload")),
        expected
    );
    assert!(tmp.path().join("trusted.json.lock").exists());
    assert_no_temp_files(tmp.path());
}

#[test]
fn readers_pick_up_changes_made_by_another_process() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = TrustStore::load(tmp.path()).expect("load");
    let a = TrustedPeer::new([1; 32], "A");
    store.add(a.clone()).expect("add");
    let mut changes = store.subscribe();

    write_trust_file_externally(tmp.path(), &[]);
    // Explicitly...
    assert!(store.reload().expect("reload"));
    assert!(!store.is_trusted(&[1; 32]));
    assert!(changes.has_changed().expect("alive"));
    changes.mark_unchanged();
    assert!(!store.reload().expect("unchanged"), "nothing new on disk");

    // ...and lazily by a reader once the reload interval passed.
    write_trust_file_externally(tmp.path(), std::slice::from_ref(&a));
    std::thread::sleep(TRUST_RELOAD_INTERVAL + std::time::Duration::from_millis(100));
    assert!(store.is_trusted(&[1; 32]));
    assert!(changes.has_changed().expect("alive"));

    // A file that cannot be parsed keeps the loaded list for readers, fails writers and
    // is never overwritten.
    std::fs::write(tmp.path().join(TRUST_FILE), "{ not json").expect("corrupt");
    std::thread::sleep(TRUST_RELOAD_INTERVAL + std::time::Duration::from_millis(100));
    assert!(store.is_trusted(&[1; 32]));
    assert!(matches!(
        store.add(TrustedPeer::new([2; 32], "B")),
        Err(CoreError::Json(_))
    ));
    assert_eq!(
        std::fs::read_to_string(tmp.path().join(TRUST_FILE)).expect("read"),
        "{ not json"
    );
}

#[test]
fn trust_roles_are_directional_and_merge() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = TrustStore::load(tmp.path()).expect("load");
    let key = [5u8; 32];
    store
        .add(TrustedPeer::paired_as(key, "Friend's hub", PeerRole::Hub))
        .expect("paired as sender with a hub");
    assert!(store.is_trusted_as(&key, PeerRole::Hub));
    assert!(
        !store.is_trusted_as(&key, PeerRole::Sender),
        "a hub we sent to must not stream into our hub"
    );
    assert!(store.is_trusted(&key));

    // Pairing in the other direction later adds the role.
    store
        .add(TrustedPeer::paired_as(key, "Friend", PeerRole::Sender))
        .expect("paired as hub");
    let peer = store.get(&hfa_proto::fingerprint(&key)).expect("peer");
    assert_eq!(peer.roles, PeerRoles::BOTH);
    assert_eq!(peer.name, "Friend");

    // Entries written before roles existed are trusted in both directions.
    let legacy = serde_json::json!({
        "version": 1,
        "peers": [{
            "device_id": hfa_proto::fingerprint(&[6; 32]),
            "name": "Old",
            "public_key": serde_json::to_value(TrustedPeer::new([6; 32], "x")).expect("json")["public_key"],
            "paired_at": 1
        }]
    });
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(TRUST_FILE), legacy.to_string()).expect("write");
    let old = TrustStore::load(dir.path()).expect("legacy");
    assert!(old.is_trusted_as(&[6; 32], PeerRole::Hub));
    assert!(old.is_trusted_as(&[6; 32], PeerRole::Sender));
}
