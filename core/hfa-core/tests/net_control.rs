//! Control channel tests over 127.0.0.1: Noise handshake, Hello, SPAKE2 pairing, trust
//! handling, message exchange, cancel safety and failure paths.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use hfa_core::control::{ControlChannel, PeerInfo};
use hfa_core::pairing::{PairingManager, DEFAULT_PAIRING_TTL, MAX_FAILED_ATTEMPTS};
use hfa_core::{CoreError, Identity, TrustStore, TrustedPeer};
use hfa_proto::control::{
    Body, Bye, Hello, PairMethod, Ping, Pong, Role, StreamAccepted, StreamStart,
};
use hfa_proto::{ControlMessage, NoiseHandshake, NoiseTransport, PairingUri, StaticKeypair};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

type Accepted = hfa_core::Result<(ControlChannel, PeerInfo)>;

/// One device: identity + trust store in its own data directory.
struct Device {
    _dir: TempDir,
    path: std::path::PathBuf,
    identity: Identity,
    trust: TrustStore,
}

fn device(name: &str) -> Device {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().to_path_buf();
    let identity = Identity::load_or_create(&path, name).expect("identity");
    let trust = TrustStore::load(&path).expect("trust");
    Device {
        _dir: dir,
        path,
        identity,
        trust,
    }
}

/// A hub listening on 127.0.0.1.
struct Hub {
    dev: Device,
    pairing: Arc<PairingManager>,
    listener: Arc<TcpListener>,
    addr: SocketAddr,
}

async fn hub() -> Hub {
    hub_with(|id| PairingManager::new(id.public_key(), id.name.clone(), 47810)).await
}

async fn hub_with(make: impl FnOnce(&Identity) -> PairingManager) -> Hub {
    let dev = device("Hub");
    let pairing = Arc::new(make(&dev.identity));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    Hub {
        dev,
        pairing,
        listener: Arc::new(listener),
        addr,
    }
}

impl Hub {
    /// Accepts the next connection in a background task.
    fn accept_one(&self) -> tokio::task::JoinHandle<Accepted> {
        let identity = self.dev.identity.clone();
        let trust = self.dev.trust.clone();
        let pairing = Arc::clone(&self.pairing);
        let listener = Arc::clone(&self.listener);
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await?;
            ControlChannel::accept(stream, &identity, &trust, &pairing).await
        })
    }
}

async fn connect(
    sender: &Device,
    addr: SocketAddr,
    expected: Option<[u8; 32]>,
    secret: Option<&str>,
) -> Accepted {
    ControlChannel::connect(
        addr,
        &sender.identity,
        &sender.trust,
        expected,
        secret.map(str::to_owned),
    )
    .await
}

/// Runs one connection attempt and returns (sender result, hub result).
async fn attempt(hub: &Hub, sender: &Device, secret: Option<&str>) -> (Accepted, Accepted) {
    let accepted = hub.accept_one();
    let connected = connect(sender, hub.addr, None, secret).await;
    let accepted = accepted.await.expect("join");
    (connected, accepted)
}

fn wrong_pin(pin: &str) -> String {
    let n: u32 = pin.parse().expect("digits");
    format!("{:06}", (n + 1) % 1_000_000)
}

#[tokio::test]
async fn first_pairing_with_pin_trusts_both_sides() {
    let hub = hub().await;
    let sender = device("Laptop");
    let info = hub.pairing.start(DEFAULT_PAIRING_TTL);

    let (connected, accepted) = attempt(&hub, &sender, Some(&info.pin)).await;
    let (_sch, hub_peer) = connected.expect("sender side");
    let (_hch, sender_peer) = accepted.expect("hub side");

    assert!(hub_peer.newly_paired && sender_peer.newly_paired);
    assert_eq!(hub_peer.device_id, hub.dev.identity.device_id);
    assert_eq!(hub_peer.name, "Hub");
    assert_eq!(hub_peer.public_key, hub.dev.identity.public_key());
    assert_eq!(hub_peer.platform, hfa_core::platform_name());
    assert_eq!(sender_peer.device_id, sender.identity.device_id);
    assert_eq!(sender_peer.name, "Laptop");
    assert_eq!(sender_peer.addr.ip(), hub.addr.ip());

    // Both trust stores were updated, in memory and on disk.
    assert!(sender.trust.is_trusted(&hub.dev.identity.public_key()));
    assert!(hub.dev.trust.is_trusted(&sender.identity.public_key()));
    let hub_on_disk = TrustStore::load(&hub.dev.path).expect("reload");
    let peer = hub_on_disk
        .get(&sender.identity.device_id)
        .expect("sender persisted");
    assert_eq!(peer.name, "Laptop");
    assert!(TrustStore::load(&sender.path)
        .expect("reload")
        .is_trusted(&hub.dev.identity.public_key()));

    // The PIN was one-time.
    assert!(hub.pairing.current().is_none());
}

#[tokio::test]
async fn wrong_pin_fails_and_trusts_nobody() {
    let hub = hub().await;
    let sender = device("Laptop");
    let info = hub.pairing.start(DEFAULT_PAIRING_TTL);

    let (connected, accepted) = attempt(&hub, &sender, Some(&wrong_pin(&info.pin))).await;
    assert!(
        matches!(connected, Err(CoreError::PairingFailed(_))),
        "{connected:?}"
    );
    assert!(
        matches!(accepted, Err(CoreError::PairingFailed(_))),
        "{accepted:?}"
    );
    assert!(sender.trust.peers().is_empty());
    assert!(hub.dev.trust.peers().is_empty());
    // One guess used, the window stays open.
    assert!(hub.pairing.current().is_some());

    // The right PIN still works afterwards.
    let (connected, accepted) = attempt(&hub, &sender, Some(&info.pin)).await;
    connected.expect("sender");
    accepted.expect("hub");
}

#[tokio::test]
async fn five_failed_attempts_close_the_window() {
    let hub = hub().await;
    let sender = device("Attacker");
    let info = hub.pairing.start(DEFAULT_PAIRING_TTL);
    for _ in 0..MAX_FAILED_ATTEMPTS {
        let (connected, _) = attempt(&hub, &sender, Some(&wrong_pin(&info.pin))).await;
        assert!(matches!(connected, Err(CoreError::PairingFailed(_))));
    }
    assert!(hub.pairing.current().is_none(), "window closed");
    // Even the right PIN is refused now, without running SPAKE2.
    let (connected, accepted) = attempt(&hub, &sender, Some(&info.pin)).await;
    assert!(matches!(connected, Err(CoreError::PairingFailed(_))));
    assert!(matches!(accepted, Err(CoreError::PairingFailed(_))));
    assert!(hub.dev.trust.peers().is_empty() && sender.trust.peers().is_empty());
}

#[tokio::test]
async fn concurrent_pair_start_is_rejected() {
    let hub = hub().await;
    let sender = device("Laptop");
    let info = hub.pairing.start(DEFAULT_PAIRING_TTL);
    // Another connection is in the middle of a pairing attempt.
    let in_flight = hub.pairing.begin_attempt(PairMethod::Pin).expect("open");

    let (connected, accepted) = attempt(&hub, &sender, Some(&info.pin)).await;
    assert!(matches!(connected, Err(CoreError::PairingFailed(_))));
    assert!(matches!(accepted, Err(CoreError::PairingFailed(_))));
    assert!(hub.dev.trust.peers().is_empty());

    drop(in_flight);
    let (connected, accepted) = attempt(&hub, &sender, Some(&info.pin)).await;
    connected.expect("sender after the other attempt ended");
    accepted.expect("hub");
}

#[tokio::test]
async fn token_pairing_via_uri_then_reconnect_without_secret() {
    let hub = hub().await;
    let sender = device("Phone");
    let info = hub.pairing.start(DEFAULT_PAIRING_TTL);
    let uri: PairingUri = info.uri.parse().expect("uri");
    assert_eq!(uri.hub_id, hub.dev.identity.public_key());
    assert_eq!(uri.token, info.token);

    let accepted = hub.accept_one();
    let (_ch, peer) = connect(&sender, hub.addr, Some(uri.hub_id), Some(&uri.token))
        .await
        .expect("token pairing");
    assert!(peer.newly_paired);
    accepted.await.expect("join").expect("hub");

    // Trusted now: no window open, no secret needed.
    assert!(hub.pairing.current().is_none());
    let accepted = hub.accept_one();
    let (_ch, peer) = connect(&sender, hub.addr, Some(uri.hub_id), None)
        .await
        .expect("reconnect");
    assert!(!peer.newly_paired);
    let (_hch, sender_peer) = accepted.await.expect("join").expect("hub");
    assert!(!sender_peer.newly_paired);
}

#[tokio::test]
async fn expected_hub_key_mismatch_is_detected_first() {
    let hub = hub().await;
    let sender = device("Laptop");
    let info = hub.pairing.start(DEFAULT_PAIRING_TTL);
    let accepted = hub.accept_one();
    let result = connect(&sender, hub.addr, Some([42; 32]), Some(&info.pin)).await;
    match result {
        Err(CoreError::KeyMismatch(id)) => assert_eq!(id, hub.dev.identity.device_id),
        other => panic!("expected KeyMismatch, got {other:?}"),
    }
    // The sender aborted inside the Noise handshake: the hub never got its key or a guess.
    assert!(accepted.await.expect("join").is_err());
    assert!(hub.dev.trust.peers().is_empty());
    assert!(hub.pairing.current().is_some());
    for _ in 0..MAX_FAILED_ATTEMPTS {
        drop(
            hub.pairing
                .begin_attempt(PairMethod::Pin)
                .expect("full budget left"),
        );
    }
}

#[tokio::test]
async fn expired_window_is_rejected() {
    let now = Arc::new(AtomicU64::new(1_000_000));
    let clock = Arc::clone(&now);
    let hub = hub_with(|id| {
        PairingManager::with_clock(
            id.public_key(),
            id.name.clone(),
            47810,
            Arc::new(move || clock.load(Ordering::SeqCst)),
        )
    })
    .await;
    let sender = device("Laptop");
    let info = hub.pairing.start(Duration::from_secs(60));
    now.fetch_add(61, Ordering::SeqCst);

    let (connected, accepted) = attempt(&hub, &sender, Some(&info.pin)).await;
    assert!(matches!(connected, Err(CoreError::PairingFailed(_))));
    assert!(matches!(accepted, Err(CoreError::PairingFailed(_))));
    assert!(hub.dev.trust.peers().is_empty());
}

#[tokio::test]
async fn unpaired_sender_without_secret_gets_pairing_required() {
    let hub = hub().await;
    let sender = device("Laptop");
    hub.pairing.start(DEFAULT_PAIRING_TTL);
    let (connected, accepted) = attempt(&hub, &sender, None).await;
    assert!(matches!(connected, Err(CoreError::PairingRequired)));
    assert!(matches!(accepted, Err(CoreError::PairingRequired)));
    // No secret, no guess used.
    assert!(hub.pairing.current().is_some());
}

#[tokio::test]
async fn sender_that_forgot_the_hub_pairs_again() {
    let hub = hub().await;
    let sender = device("Laptop");
    let info = hub.pairing.start(DEFAULT_PAIRING_TTL);
    let (c, a) = attempt(&hub, &sender, Some(&info.pin)).await;
    c.expect("paired");
    a.expect("paired");
    // The sender forgets the hub; the hub still trusts the sender.
    assert!(sender
        .trust
        .remove(&hub.dev.identity.device_id)
        .expect("remove"));

    // Without a secret the sender refuses to continue (and the hub agrees).
    let (c, a) = attempt(&hub, &sender, None).await;
    assert!(matches!(c, Err(CoreError::PairingRequired)), "{c:?}");
    assert!(matches!(a, Err(CoreError::PairingRequired)), "{a:?}");

    // With a fresh PIN the pair is re-established.
    let info = hub.pairing.start(DEFAULT_PAIRING_TTL);
    let (c, a) = attempt(&hub, &sender, Some(&info.pin)).await;
    assert!(c.expect("sender").1.newly_paired);
    assert!(a.expect("hub").1.newly_paired);
    assert!(sender.trust.is_trusted(&hub.dev.identity.public_key()));
}

// ---------------------------------------------------------------------------------------
// A hand-written peer speaking the raw wire protocol (records + Noise), to test either side
// against a misbehaving peer.

async fn write_record(s: &mut TcpStream, payload: &[u8]) {
    let len = u16::try_from(payload.len()).expect("fits");
    s.write_all(&len.to_be_bytes()).await.expect("write");
    s.write_all(payload).await.expect("write");
}

async fn read_record(s: &mut TcpStream) -> Vec<u8> {
    let mut len = [0u8; 2];
    s.read_exact(&mut len).await.expect("len");
    let mut buf = vec![0u8; usize::from(u16::from_be_bytes(len))];
    s.read_exact(&mut buf).await.expect("record");
    buf
}

struct RawPeer {
    stream: TcpStream,
    transport: NoiseTransport,
    keys: StaticKeypair,
}

impl RawPeer {
    /// Connects to a hub and runs the initiator side of the Noise handshake (a raw sender).
    async fn connect(addr: SocketAddr, keys: StaticKeypair) -> RawPeer {
        let mut stream = TcpStream::connect(addr).await.expect("connect");
        let mut hs = NoiseHandshake::initiator(&keys).expect("initiator");
        let m1 = hs.write_message(&[]).expect("m1");
        write_record(&mut stream, &m1).await;
        hs.read_message(&read_record(&mut stream).await)
            .expect("m2");
        let m3 = hs.write_message(&[]).expect("m3");
        write_record(&mut stream, &m3).await;
        RawPeer {
            stream,
            transport: hs.into_transport().expect("transport"),
            keys,
        }
    }

    /// As a raw sender: sends a sender `Hello` and returns the hub's.
    async fn sender_hello(&mut self, pairing_required: bool) -> Hello {
        let hello = Hello {
            protocol_version: 0,
            device_id: hfa_proto::fingerprint(&self.keys.public),
            device_name: "Raw sender".into(),
            platform: "test".into(),
            app_version: "0".into(),
            role: Role::Sender as i32,
            pairing_required,
        };
        self.send(&Body::Hello(hello).into()).await;
        let Some(Body::Hello(hub_hello)) = self.recv().await.body else {
            panic!("expected the hub's Hello");
        };
        hub_hello
    }

    /// Encrypts `plain` as one record and writes it.
    async fn send_plain(&mut self, plain: &[u8]) {
        let record = self.transport.encrypt(plain).expect("encrypt");
        write_record(&mut self.stream, &record).await;
    }

    /// Accepts one connection and runs the responder side of the Noise handshake.
    async fn accept(listener: &TcpListener, keys: StaticKeypair) -> RawPeer {
        let (mut stream, _) = listener.accept().await.expect("accept");
        let mut hs = NoiseHandshake::responder(&keys).expect("responder");
        hs.read_message(&read_record(&mut stream).await)
            .expect("m1");
        let m2 = hs.write_message(&[]).expect("m2");
        write_record(&mut stream, &m2).await;
        hs.read_message(&read_record(&mut stream).await)
            .expect("m3");
        RawPeer {
            stream,
            transport: hs.into_transport().expect("transport"),
            keys,
        }
    }

    fn seal(&mut self, msg: &ControlMessage) -> Vec<u8> {
        let frame = hfa_proto::encode_frame(msg).expect("frame");
        self.transport.encrypt(&frame).expect("encrypt")
    }

    async fn send(&mut self, msg: &ControlMessage) {
        let record = self.seal(msg);
        write_record(&mut self.stream, &record).await;
    }

    async fn recv(&mut self) -> ControlMessage {
        let record = read_record(&mut self.stream).await;
        let plain = self.transport.decrypt(&record).expect("decrypt");
        hfa_proto::decode_message(&plain[hfa_proto::control::FRAME_PREFIX_LEN..]).expect("decode")
    }

    /// Reads the sender's `Hello` and answers with a hub `Hello`.
    async fn hello(&mut self, pairing_required: bool) -> Hello {
        let Some(Body::Hello(sender_hello)) = self.recv().await.body else {
            panic!("expected Hello");
        };
        let hub_hello = Hello {
            protocol_version: 0,
            device_id: hfa_proto::fingerprint(&self.keys.public),
            device_name: "Raw hub".into(),
            platform: "test".into(),
            app_version: "0".into(),
            role: Role::Hub as i32,
            pairing_required,
        };
        self.send(&Body::Hello(hub_hello).into()).await;
        sender_hello
    }
}

#[tokio::test]
async fn sender_refuses_an_untrusted_hub_that_claims_no_pairing_is_needed() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let sender = device("Laptop");
    let rogue = tokio::spawn(async move {
        let keys = StaticKeypair::generate().expect("keys");
        let mut raw = RawPeer::accept(&listener, keys).await;
        let hello = raw.hello(false).await;
        // The sender tells the hub up front that it does not trust it...
        assert!(hello.pairing_required);
        // ...and then leaves instead of streaming.
        let next = raw.recv().await;
        assert!(matches!(next.body, Some(Body::Bye(_))), "{next:?}");
    });
    let result = connect(&sender, addr, None, None).await;
    assert!(
        matches!(result, Err(CoreError::PairingRequired)),
        "{result:?}"
    );
    rogue.await.expect("rogue hub assertions");
    assert!(sender.trust.peers().is_empty());
}

#[tokio::test]
async fn rogue_hub_without_the_secret_cannot_complete_pairing() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let sender = device("Laptop");
    let rogue = tokio::spawn(async move {
        let keys = StaticKeypair::generate().expect("keys");
        let mut raw = RawPeer::accept(&listener, keys).await;
        raw.hello(false).await;
        let Some(Body::PairStart(_)) = raw.recv().await.body else {
            panic!("expected PairStart");
        };
        // Guess a PIN and play along.
        let (session, msg) =
            hfa_proto::PairingSession::start("000000", &raw.transport.handshake_hash());
        raw.send(&Body::PairSpake(hfa_proto::control::PairSpake { msg }).into())
            .await;
        let Some(Body::PairSpake(peer)) = raw.recv().await.body else {
            panic!("expected PairSpake");
        };
        let key = session.finish(&peer.msg).expect("finish");
        let Some(Body::PairConfirm(_)) = raw.recv().await.body else {
            panic!("expected PairConfirm");
        };
        raw.send(
            &Body::PairConfirm(hfa_proto::control::PairConfirm {
                mac: key.confirm_mac(hfa_proto::PairingRole::Hub).to_vec(),
            })
            .into(),
        )
        .await;
        raw.send(
            &Body::PairResult(hfa_proto::control::PairResult {
                ok: true,
                reason: String::new(),
            })
            .into(),
        )
        .await;
    });
    let result = connect(&sender, addr, None, Some("123456")).await;
    assert!(
        matches!(result, Err(CoreError::PairingFailed(_))),
        "{result:?}"
    );
    rogue.await.expect("rogue");
    assert!(
        sender.trust.peers().is_empty(),
        "rogue hub must not be trusted"
    );
}

/// A sender that trusts `keys` and is connected to a raw hub.
async fn sender_with_raw_hub() -> (Device, ControlChannel, RawPeer) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let keys = StaticKeypair::generate().expect("keys");
    let sender = device("Laptop");
    sender
        .trust
        .add(TrustedPeer::new(keys.public, "Raw hub"))
        .expect("trust");
    let raw = tokio::spawn(async move {
        let mut raw = RawPeer::accept(&listener, keys).await;
        let hello = raw.hello(false).await;
        assert!(!hello.pairing_required);
        raw
    });
    let (ch, peer) = connect(&sender, addr, None, None).await.expect("connect");
    assert!(!peer.newly_paired);
    (sender, ch, raw.await.expect("raw"))
}

#[tokio::test]
async fn recv_is_cancel_safe_mid_record() {
    let (_sender, mut ch, mut raw) = sender_with_raw_hub().await;
    let first = raw.seal(&Body::Ping(Ping { nonce: 1, t_us: 10 }).into());
    let second = raw.seal(&Body::Ping(Ping { nonce: 2, t_us: 20 }).into());
    let mut wire = Vec::new();
    for rec in [&first, &second] {
        wire.extend_from_slice(&u16::try_from(rec.len()).expect("len").to_be_bytes());
        wire.extend_from_slice(rec);
    }
    // Deliver the first record in pieces; a pending `recv` is dropped after each piece.
    let cut1 = 1; // inside the length prefix
    let cut2 = 7; // inside the ciphertext
    for (from, to) in [(0, cut1), (cut1, cut2)] {
        raw.stream.write_all(&wire[from..to]).await.expect("write");
        raw.stream.flush().await.expect("flush");
        let pending = tokio::time::timeout(Duration::from_millis(50), ch.recv()).await;
        assert!(pending.is_err(), "no complete record yet");
    }
    // The rest of the first record and all of the second in one go.
    raw.stream.write_all(&wire[cut2..]).await.expect("write");
    let m1 = ch.recv().await.expect("first");
    let m2 = ch.recv().await.expect("second");
    assert_eq!(m1.body, Some(Body::Ping(Ping { nonce: 1, t_us: 10 })));
    assert_eq!(m2.body, Some(Body::Ping(Ping { nonce: 2, t_us: 20 })));

    // Both directions still work (the Noise nonces are in sync).
    ch.send(&Body::Pong(Pong { nonce: 2, t_us: 20 }).into())
        .await
        .expect("send");
    assert_eq!(
        raw.recv().await.body,
        Some(Body::Pong(Pong { nonce: 2, t_us: 20 }))
    );
}

#[tokio::test]
async fn oversized_frame_poisons_and_closes_the_channel() {
    let (_sender, mut ch, mut raw) = sender_with_raw_hub().await;
    // A frame header announcing more than MAX_CONTROL_FRAME bytes.
    let bogus = u32::try_from(hfa_proto::MAX_CONTROL_FRAME + 1)
        .expect("fits")
        .to_be_bytes();
    let record = raw.transport.encrypt(&bogus).expect("encrypt");
    write_record(&mut raw.stream, &record).await;
    let err = ch.recv().await.expect_err("poisoned");
    assert!(
        matches!(
            err,
            CoreError::Proto(hfa_proto::ProtoError::FrameTooLarge { .. })
        ),
        "{err:?}"
    );
    assert!(ch.is_closed());
    // Later messages are not delivered and sending is refused.
    raw.send(&Body::Ping(Ping::default()).into()).await;
    assert!(ch.recv().await.is_err());
    assert!(ch.send(&Body::Ping(Ping::default()).into()).await.is_err());
}

#[tokio::test]
async fn tampered_record_closes_the_channel() {
    let (_sender, mut ch, mut raw) = sender_with_raw_hub().await;
    let mut record = raw.seal(&Body::Ping(Ping::default()).into());
    record[3] ^= 0x40;
    write_record(&mut raw.stream, &record).await;
    let err = ch.recv().await.expect_err("tampered");
    assert!(matches!(err, CoreError::Proto(_)), "{err:?}");
    assert!(ch.is_closed());
}

#[tokio::test]
async fn peer_closing_gives_closed() {
    let (_sender, mut ch, raw) = sender_with_raw_hub().await;
    drop(raw);
    assert_eq!(ch.recv().await.expect_err("eof"), CoreError::Closed);
    assert_eq!(
        ch.recv().await.expect_err("still closed"),
        CoreError::Closed
    );
}

#[tokio::test]
async fn close_sends_bye() {
    let (_sender, ch, mut raw) = sender_with_raw_hub().await;
    ch.close("going away").await.expect("close");
    assert_eq!(
        raw.recv().await.body,
        Some(Body::Bye(Bye {
            reason: "going away".into()
        }))
    );
    let mut rest = Vec::new();
    raw.stream.read_to_end(&mut rest).await.expect("eof");
    assert!(rest.is_empty());
}

#[tokio::test]
async fn messages_flow_both_ways_with_a_select_loop() {
    let hub = hub().await;
    let sender = device("Laptop");
    let info = hub.pairing.start(DEFAULT_PAIRING_TTL);
    let accepted = hub.accept_one();
    let (mut sch, _) = connect(&sender, hub.addr, None, Some(&info.pin))
        .await
        .expect("connect");
    let (mut hch, _) = accepted.await.expect("join").expect("accept");

    // Hub engine style: one task owns the channel; recv races a fast ticker and a command
    // queue, so pending `recv` futures are dropped all the time.
    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::channel::<ControlMessage>(8);
    let hub_task = tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(3));
        let (mut pings, mut pongs, mut nonce) = (0u32, 0u32, 0u64);
        loop {
            tokio::select! {
                msg = hch.recv() => match msg.expect("hub recv").body {
                    Some(Body::StreamStart(s)) => {
                        let reply = Body::StreamAccepted(StreamAccepted { stream_id: s.stream_id, udp_port: 47810 });
                        hch.send(&reply.into()).await.expect("send accepted");
                    }
                    Some(Body::Pong(p)) => {
                        assert!(p.nonce >= 1 && p.nonce <= nonce);
                        pongs += 1;
                    }
                    Some(Body::Bye(b)) => {
                        assert_eq!(b.reason, "done");
                        return (pings, pongs);
                    }
                    other => panic!("unexpected {other:?}"),
                },
                _ = tick.tick() => {
                    if pings < 200 {
                        nonce += 1;
                        pings += 1;
                        hch.send(&Body::Ping(Ping { nonce, t_us: nonce * 10 }).into()).await.expect("ping");
                    }
                }
                Some(cmd) = cmd_rx.recv() => hch.send(&cmd).await.expect("command"),
            }
        }
    });

    sch.send(
        &Body::StreamStart(StreamStart {
            stream_id: 7,
            sample_rate: 48_000,
            channels: 2,
            frame_ms: 10,
            bitrate: 128_000,
            label: "System".into(),
            media_key: vec![1; 32],
        })
        .into(),
    )
    .await
    .expect("stream start");
    cmd_tx
        .send(
            Body::Bye(Bye {
                reason: "from command".into(),
            })
            .into(),
        )
        .await
        .expect("cmd");

    let (mut accepted, mut commands, mut answered) = (false, false, 0u32);
    while answered < 50 || !accepted || !commands {
        match sch.recv().await.expect("sender recv").body {
            Some(Body::StreamAccepted(a)) => {
                assert_eq!(a.stream_id, 7);
                accepted = true;
            }
            Some(Body::Ping(p)) => {
                sch.send(
                    &Body::Pong(Pong {
                        nonce: p.nonce,
                        t_us: p.t_us,
                    })
                    .into(),
                )
                .await
                .expect("pong");
                answered += 1;
            }
            Some(Body::Bye(b)) => {
                assert_eq!(b.reason, "from command");
                commands = true;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    // Keep the channel open until the hub has read everything (a closed socket could turn
    // the hub's next ping into a reset that discards unread data).
    sch.send(
        &Body::Bye(Bye {
            reason: "done".into(),
        })
        .into(),
    )
    .await
    .expect("bye");
    let (pings, pongs) = hub_task.await.expect("hub task");
    drop(sch);
    assert!(pings >= 50);
    assert_eq!(pongs, answered, "every pong arrived exactly once");
}

#[tokio::test(start_paused = true)]
async fn silent_peer_times_out() {
    // A hub whose sender never speaks.
    let hub = hub().await;
    let accepted = hub.accept_one();
    let _silent = TcpStream::connect(hub.addr).await.expect("connect");
    let result = accepted.await.expect("join");
    assert!(matches!(result, Err(CoreError::Timeout(_))), "{result:?}");

    // A sender whose hub accepts TCP but never answers the handshake.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let silent_hub = tokio::spawn(async move {
        let (s, _) = listener.accept().await.expect("accept");
        tokio::time::sleep(Duration::from_secs(3600)).await;
        drop(s);
    });
    let sender = device("Laptop");
    let result = connect(&sender, addr, None, None).await;
    assert!(matches!(result, Err(CoreError::Timeout(_))), "{result:?}");
    silent_hub.abort();
}

/// Runs a raw sender through SPAKE2 with the right PIN, lets `interfere` act on the hub's
/// pairing manager right before the sender's confirmation MAC, and returns the hub's reply.
async fn pair_raw_sender_with_interference(
    hub: &Hub,
    pin: &str,
    interfere: impl FnOnce(&PairingManager),
) -> (ControlMessage, Accepted) {
    let accepted = hub.accept_one();
    let keys = StaticKeypair::generate().expect("keys");
    let mut raw = RawPeer::connect(hub.addr, keys).await;
    assert!(raw.sender_hello(true).await.pairing_required);
    raw.send(
        &Body::PairStart(hfa_proto::control::PairStart {
            method: PairMethod::Pin as i32,
        })
        .into(),
    )
    .await;
    // The hub answers only after `begin_attempt`, so the attempt is in flight now.
    let Some(Body::PairSpake(hub_msg)) = raw.recv().await.body else {
        panic!("expected PairSpake");
    };
    let (session, msg) = hfa_proto::PairingSession::start(pin, &raw.transport.handshake_hash());
    raw.send(&Body::PairSpake(hfa_proto::control::PairSpake { msg }).into())
        .await;
    let key = session.finish(&hub_msg.msg).expect("finish");
    interfere(&hub.pairing);
    raw.send(
        &Body::PairConfirm(hfa_proto::control::PairConfirm {
            mac: key.confirm_mac(hfa_proto::PairingRole::Sender).to_vec(),
        })
        .into(),
    )
    .await;
    let reply = raw.recv().await;
    (reply, accepted.await.expect("join"))
}

#[tokio::test]
async fn cancel_or_new_window_mid_pairing_trusts_nobody() {
    for replace in [false, true] {
        let hub = hub().await;
        let info = hub.pairing.start(DEFAULT_PAIRING_TTL);
        let (reply, accepted) = pair_raw_sender_with_interference(&hub, &info.pin, |p| {
            if replace {
                p.start(DEFAULT_PAIRING_TTL);
            } else {
                p.cancel();
            }
        })
        .await;
        assert!(
            matches!(&reply.body, Some(Body::PairResult(r)) if !r.ok),
            "{reply:?}"
        );
        assert!(
            matches!(accepted, Err(CoreError::PairingFailed(_))),
            "{accepted:?}"
        );
        assert!(
            hub.dev.trust.peers().is_empty(),
            "nothing trusted in memory"
        );
        assert!(
            TrustStore::load(&hub.dev.path)
                .expect("reload")
                .peers()
                .is_empty(),
            "nothing trusted on disk"
        );
        // A replacement window is untouched; a cancelled one stays closed.
        assert_eq!(hub.pairing.current().is_some(), replace);
    }
}

#[tokio::test]
async fn right_pin_with_the_window_still_open_pairs_the_raw_sender() {
    // Control case for the test above: the same raw sender without interference pairs.
    let hub = hub().await;
    let info = hub.pairing.start(DEFAULT_PAIRING_TTL);
    let (reply, accepted) = pair_raw_sender_with_interference(&hub, &info.pin, |_| {}).await;
    assert!(
        matches!(reply.body, Some(Body::PairConfirm(_))),
        "{reply:?}"
    );
    let (_ch, peer) = accepted.expect("paired");
    assert!(peer.newly_paired);
    assert!(hub.dev.trust.is_trusted(&peer.public_key));
    assert!(hub.pairing.current().is_none(), "one-time secret consumed");
}

/// `n` empty-body frames (`00 00 00 00` each), which decode to no message.
fn empty_frames(n: usize) -> Vec<u8> {
    vec![0u8; n * hfa_proto::control::FRAME_PREFIX_LEN]
}

#[tokio::test]
async fn undecodable_frames_during_the_handshake_are_fatal() {
    // Hub side: a raw sender sends garbage frames instead of its Hello.
    let hub = hub().await;
    let accepted = hub.accept_one();
    let mut raw = RawPeer::connect(hub.addr, StaticKeypair::generate().expect("keys")).await;
    raw.send_plain(&empty_frames(16_000)).await;
    let result = tokio::time::timeout(Duration::from_secs(5), accepted)
        .await
        .expect("rejected right away, not at the handshake timeout")
        .expect("join");
    assert!(matches!(result, Err(CoreError::Protocol(_))), "{result:?}");

    // Sender side: a raw hub answers the Hello with a garbage frame.
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let sender = device("Laptop");
    let rogue = tokio::spawn(async move {
        let mut raw = RawPeer::accept(&listener, StaticKeypair::generate().expect("keys")).await;
        let _ = raw.recv().await; // the sender's Hello
        raw.send_plain(&empty_frames(1)).await;
        raw
    });
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        connect(&sender, addr, None, Some("123456")),
    )
    .await
    .expect("rejected right away");
    assert!(matches!(result, Err(CoreError::Protocol(_))), "{result:?}");
    drop(rogue.await.expect("rogue"));
}

#[tokio::test]
async fn a_few_undecodable_frames_are_skipped_but_a_flood_closes_the_channel() {
    let (_sender, mut ch, mut raw) = sender_with_raw_hub().await;
    // A few unknown frames (e.g. from a newer peer) are skipped.
    let mut plain = empty_frames(hfa_core::control::MAX_SKIPPED_FRAMES as usize);
    plain.extend(
        hfa_proto::encode_frame(&Body::Ping(Ping { nonce: 5, t_us: 0 }).into()).expect("frame"),
    );
    raw.send_plain(&plain).await;
    assert_eq!(
        ch.recv().await.expect("ping after skipped frames").body,
        Some(Body::Ping(Ping { nonce: 5, t_us: 0 }))
    );
    // The counter restarts after a good frame; a flood in one record closes the channel.
    raw.send_plain(&empty_frames(16_000)).await;
    let err = ch.recv().await.expect_err("flood");
    assert!(matches!(err, CoreError::Protocol(_)), "{err:?}");
    assert!(ch.is_closed());
}

#[tokio::test]
async fn empty_records_close_the_channel() {
    let (_sender, mut ch, mut raw) = sender_with_raw_hub().await;
    for _ in 0..3 {
        raw.send_plain(&[]).await;
    }
    let err = ch.recv().await.expect_err("empty record");
    assert!(matches!(err, CoreError::Protocol(_)), "{err:?}");
    assert!(ch.is_closed());
}

#[tokio::test]
async fn many_small_records_in_one_read_arrive_in_order() {
    let (_sender, mut ch, mut raw) = sender_with_raw_hub().await;
    let mut wire = Vec::new();
    for nonce in 0..2_000u64 {
        let record = raw.seal(&Body::Ping(Ping { nonce, t_us: 0 }).into());
        wire.extend_from_slice(&u16::try_from(record.len()).expect("len").to_be_bytes());
        wire.extend_from_slice(&record);
    }
    raw.stream.write_all(&wire).await.expect("write");
    for nonce in 0..2_000u64 {
        assert_eq!(
            ch.recv().await.expect("ping").body,
            Some(Body::Ping(Ping { nonce, t_us: 0 }))
        );
    }
}

/// Loop protection: a device never connects to its own hub (same identity), whether the
/// sender notices it (the normal client) or only the hub does (a client that does not check).
#[tokio::test]
async fn a_device_cannot_connect_to_its_own_hub() {
    let hub = hub().await;
    let pin = hub.pairing.start(DEFAULT_PAIRING_TTL).pin;
    let accepted = hub.accept_one();
    let connected = ControlChannel::connect(
        hub.addr,
        &hub.dev.identity,
        &hub.dev.trust,
        None,
        Some(pin.clone()),
    )
    .await;
    assert_eq!(connected.err(), Some(CoreError::SelfConnection));
    assert!(accepted.await.expect("join").is_err());

    // A raw client with the hub's own key completes the Noise handshake: the hub refuses it.
    let accepted = hub.accept_one();
    let _raw = RawPeer::connect(hub.addr, hub.dev.identity.keypair.clone()).await;
    assert_eq!(
        accepted.await.expect("join").err(),
        Some(CoreError::SelfConnection)
    );
    assert!(hub.dev.trust.peers().is_empty(), "nothing was paired");
    assert!(
        hub.pairing.current().is_some(),
        "the window was not used up"
    );
}
