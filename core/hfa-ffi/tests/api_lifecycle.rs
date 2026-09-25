//! End-to-end check of the flutter_rust_bridge API against the real engines: init, settings,
//! trust store sharing, hub start / pairing / stop, and a sender that cannot reach its hub.
//!
//! Everything runs in one test because the API uses one global engine manager per process.
//! `api_e2e.rs` covers a sender that pairs with and streams to a hub.

use hfa_core::{TrustStore, TrustedPeer};
use hfa_ffi::api::app::{forget_peer, get_settings, init_app, trusted_peers, update_settings};
use hfa_ffi::api::hub::{
    hub_cancel_pairing, hub_pairing_status, hub_sources, hub_start, hub_start_pairing, hub_status,
    hub_stop,
};
use hfa_ffi::api::sender::{
    sender_start, sender_status, sender_stop, CaptureSourceDto, SenderStartDto,
};

#[test]
fn api_lifecycle_with_real_engines() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A null output: the test machine may have no audio device. Port 0: any free port.
    std::fs::write(
        dir.path().join("settings.json"),
        r#"{"device_name": "Test hub", "port": 0, "output": "Null"}"#,
    )
    .expect("write settings");
    let data_dir = dir.path().to_string_lossy().into_owned();

    let info = init_app(data_dir.clone(), Some("ignored: settings exist".into())).expect("init");
    assert_eq!(info.device_name, "Test hub");
    assert!(
        hfa_proto::is_fingerprint(&info.device_id),
        "{}",
        info.device_id
    );
    assert_eq!(
        init_app(data_dir, None).expect("idempotent").device_id,
        info.device_id
    );

    assert!(trusted_peers().expect("peers").is_empty());

    // Pairings saved by someone else (as the engines do) are seen, and forgetting another
    // peer keeps them: trust is never cached by the API.
    let store = TrustStore::load(dir.path()).expect("trust store");
    for (key, name) in [([7u8; 32], "Laptop"), ([8u8; 32], "Phone")] {
        store
            .add(TrustedPeer {
                device_id: hfa_proto::fingerprint(&key),
                name: name.into(),
                public_key: key,
                paired_at: 1_700_000_000,
                roles: hfa_core::PeerRoles::BOTH,
            })
            .expect("add peer");
    }
    assert_eq!(trusted_peers().expect("peers").len(), 2);
    forget_peer(hfa_proto::fingerprint(&[7u8; 32])).expect("forget");
    let peers = trusted_peers().expect("peers");
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0].name, "Phone");
    forget_peer(hfa_proto::fingerprint(&[8u8; 32])).expect("forget");
    assert!(TrustStore::load(dir.path())
        .expect("trust store")
        .peers()
        .is_empty());

    let status = hub_start().expect("hub starts");
    assert!(status.running);
    assert_ne!(status.port, 0);
    assert_eq!(hub_start().expect("idempotent").port, status.port);
    assert_eq!(hub_status(), status);
    // Advertising is owned by the FFI: either it runs or the status says why not.
    assert_eq!(
        status.advertised,
        status.advertise_error.is_none(),
        "{status:?}"
    );
    assert!(hub_sources().is_empty());
    assert_eq!(hub_pairing_status().expect("pairing status"), None);
    let pairing = hub_start_pairing().expect("pairing window");
    assert_eq!(pairing.pin.len(), 6);
    assert!(pairing.uri.starts_with("hfa://pair?"));
    assert_eq!(
        hub_pairing_status().expect("pairing status"),
        Some(pairing.clone())
    );
    hub_cancel_pairing().expect("cancel");
    assert_eq!(hub_pairing_status().expect("pairing status"), None);

    // A sender may not target its own hub.
    let own = SenderStartDto {
        hub_host: "127.0.0.1".into(),
        hub_port: status.port,
        hub_device_id: Some(info.device_id.clone()),
        hub_key: None,
        pairing_secret: None,
        source: CaptureSourceDto::Tone { freq_hz: 440.0 },
        label: String::new(),
    };
    assert!(sender_start(own).is_err());
    hub_stop().expect("hub stops");
    assert!(!hub_status().running);
    assert!(!hub_status().advertised);
    assert_eq!(hub_pairing_status().expect("pairing status"), None);

    // Only after the hub ran: the settings DTO has no form for the Null output, so a round
    // trip turns it into the OS default output (absent on headless CI machines).
    let mut settings = get_settings().expect("settings");
    settings.bitrate = 96_000;
    update_settings(settings).expect("update");
    assert_eq!(get_settings().expect("settings").bitrate, 96_000);

    // A sender towards a closed port keeps trying (it never reaches `streaming`).
    let closed = SenderStartDto {
        hub_host: "127.0.0.1".into(),
        hub_port: status.port,
        hub_device_id: None,
        hub_key: None,
        pairing_secret: Some("123456".into()),
        source: CaptureSourceDto::Tone { freq_hz: 440.0 },
        label: "Tone".into(),
    };
    sender_start(closed.clone()).expect("sender starts");
    assert!(sender_start(closed).is_err(), "only one sender at a time");
    assert_ne!(sender_status().state, "streaming");
    sender_stop().expect("sender stops");
    assert_eq!(sender_status().state, "idle");
}
