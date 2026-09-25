//! UDP media tests over 127.0.0.1: sealing, sending from a plain thread, demultiplexing and
//! rejection of replayed, duplicated, foreign and malformed datagrams.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use hfa_core::media::{MediaDemux, MediaSender, MAX_MEDIA_PAYLOAD, MAX_SEQ_JUMP};
use hfa_core::CoreError;
use hfa_proto::{
    MediaHeader, MediaKey, MediaSealer, ProtoError, FLAG_DTX, FLAG_RESET, REPLAY_WINDOW,
};
use tokio::net::UdpSocket;

async fn socket() -> Arc<UdpSocket> {
    Arc::new(UdpSocket::bind("127.0.0.1:0").await.expect("bind"))
}

async fn recv(socket: &UdpSocket) -> Vec<u8> {
    let mut buf = vec![0u8; 2048];
    let (n, _) = tokio::time::timeout(Duration::from_secs(5), socket.recv_from(&mut buf))
        .await
        .expect("datagram in time")
        .expect("recv");
    buf.truncate(n);
    buf
}

/// Seals a datagram with an explicit sequence number (what a sender holding the key could
/// produce; used to test the window rules on authentic packets).
fn sealed(key: &MediaKey, stream_id: u32, seq: u32, payload: &[u8]) -> Vec<u8> {
    let header = MediaHeader {
        flags: 0,
        stream_id,
        seq,
        timestamp: seq.wrapping_mul(480),
    };
    let mut out = Vec::new();
    MediaSealer::new(key, stream_id)
        .seal(&header, payload, &mut out)
        .expect("seal");
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn roundtrip_from_a_plain_thread() {
    let hub = socket().await;
    let hub_addr: SocketAddr = hub.local_addr().expect("addr");
    let mut sender = MediaSender::new(socket().await, hub_addr, 0xABCD);
    assert_eq!(sender.stream_id(), 0xABCD);
    assert_eq!(sender.next_seq(), Some(0));
    assert_eq!(sender.dest(), hub_addr);
    let mut demux = MediaDemux::new();
    assert!(demux.add_stream(0xABCD, sender.key()));

    // The encoder thread is not a tokio thread: `send` must work (and never block) there.
    let encoder = std::thread::spawn(move || {
        let mut sent = 0;
        for i in 0u32..20 {
            let flags = match i {
                5 => FLAG_DTX,
                10 => FLAG_RESET,
                _ => 0,
            };
            let payload = if flags == FLAG_DTX {
                Vec::new()
            } else {
                vec![u8::try_from(i).expect("small"); 100 + i as usize]
            };
            // Even the very first packet goes out: no reactor readiness is involved.
            assert!(sender.send(flags, i * 480, &payload).expect("send"));
            sent += 1;
            std::thread::sleep(Duration::from_millis(1));
        }
        (sent, sender.next_seq())
    });

    let mut seen = Vec::new();
    while seen.len() < 20 {
        let datagram = recv(&hub).await;
        let (header, payload) = demux.open(&datagram).expect("authentic");
        assert_eq!(header.stream_id, 0xABCD);
        assert_eq!(header.timestamp, header.seq * 480);
        match header.seq {
            5 => assert!(header.has_flag(FLAG_DTX) && payload.is_empty()),
            10 => assert!(header.has_flag(FLAG_RESET)),
            s => assert_eq!(
                payload,
                vec![u8::try_from(s).expect("small"); 100 + s as usize]
            ),
        }
        seen.push(header.seq);
    }
    let (sent, next) = encoder.join().expect("encoder");
    assert_eq!(sent, 20);
    assert_eq!(next, Some(20));
    seen.sort_unstable();
    assert_eq!(
        seen,
        (0..20).collect::<Vec<u32>>(),
        "every seq exactly once"
    );
    assert_eq!(demux.stats().accepted, 20);
    assert_eq!(demux.stats().rejected(), 0);
    assert_eq!(demux.highest_seq(0xABCD), seen.last().copied());
}

#[tokio::test]
async fn duplicates_and_replays_are_rejected() {
    let hub = socket().await;
    let mut sender = MediaSender::new(socket().await, hub.local_addr().expect("addr"), 7);
    let mut demux = MediaDemux::new();
    assert!(demux.add_stream(7, sender.key()));

    let mut captured = Vec::new();
    for i in 0..3u8 {
        assert!(sender.send(0, 0, &[i; 10]).expect("send"));
        captured.push(recv(&hub).await);
    }
    for d in &captured {
        demux.open(d).expect("fresh");
    }
    // Replaying any captured datagram fails before it could reach a jitter buffer.
    for d in &captured {
        let err = demux.open(d).expect_err("replay");
        assert!(
            matches!(err, CoreError::Proto(ProtoError::Replay { .. })),
            "{err:?}"
        );
    }
    assert_eq!(demux.stats().out_of_window, 3);
}

#[test]
fn sequence_window_rules() {
    let key = MediaKey::generate();
    let mut demux = MediaDemux::new();
    assert!(demux.add_stream(1, &key));

    // Before the first packet a stream is expected to start near seq 0.
    let too_far_first = sealed(&key, 1, MAX_SEQ_JUMP + 1, b"x");
    assert!(matches!(
        demux.open(&too_far_first),
        Err(CoreError::Proto(ProtoError::Replay { .. }))
    ));
    demux.open(&sealed(&key, 1, 100, b"a")).expect("start");

    // Far ahead of the highest accepted seq: rejected without decryption, window unchanged.
    let far = sealed(&key, 1, 100 + MAX_SEQ_JUMP + 1, b"far");
    assert!(demux.open(&far).is_err());
    assert_eq!(demux.highest_seq(1), Some(100));
    // At the edge: accepted.
    demux
        .open(&sealed(&key, 1, 100 + MAX_SEQ_JUMP, b"edge"))
        .expect("edge of the window");
    let high = 100 + MAX_SEQ_JUMP;

    // Reordering inside the replay window is fine, once.
    let late = sealed(&key, 1, high - 5, b"late");
    demux.open(&late).expect("reordered");
    assert!(demux.open(&late).is_err(), "duplicate");
    // Older than the replay window: rejected even though authentic.
    let ancient = sealed(&key, 1, high - REPLAY_WINDOW, b"old");
    assert!(matches!(
        demux.open(&ancient),
        Err(CoreError::Proto(ProtoError::Replay { .. }))
    ));
    let stats = demux.stats();
    assert_eq!(stats.accepted, 3);
    assert_eq!(stats.out_of_window, 4);
}

#[test]
fn foreign_and_malformed_datagrams_are_rejected() {
    let key_a = MediaKey::generate();
    let key_b = MediaKey::generate();
    let mut demux = MediaDemux::new();
    assert!(demux.add_stream(1, &key_a));
    assert!(demux.add_stream(2, &key_b));
    assert!(!demux.add_stream(2, &key_a), "no hijacking of stream 2");
    assert_eq!(demux.len(), 2);

    // Unknown stream.
    assert_eq!(
        demux.open(&sealed(&key_a, 3, 0, b"x")),
        Err(CoreError::UnknownStream(3))
    );
    // Stream 2's id sealed with stream 1's key (another sender trying to inject).
    assert_eq!(
        demux.open(&sealed(&key_a, 2, 0, b"x")),
        Err(CoreError::Proto(ProtoError::Crypto))
    );
    // Tampered ciphertext.
    let mut tampered = sealed(&key_a, 1, 0, b"hello");
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    assert_eq!(
        demux.open(&tampered),
        Err(CoreError::Proto(ProtoError::Crypto))
    );
    // A forged packet does not burn its seq: the authentic one is still accepted.
    demux
        .open(&sealed(&key_a, 1, 0, b"hello"))
        .expect("authentic");
    // Garbage, truncated and oversized datagrams.
    assert!(demux.open(b"GARBAGE-GARBAGE-GARBAGE-GARBAGE!!").is_err());
    assert!(demux.open(&sealed(&key_a, 1, 1, b"")[..20]).is_err());
    let mut huge = sealed(&key_a, 1, 1, &[0u8; 1000]);
    huge.resize(hfa_proto::MAX_DATAGRAM + 1, 0);
    assert!(demux.open(&huge).is_err());
    demux.open(&sealed(&key_a, 1, 1, b"")).expect("still fine");

    let stats = demux.stats();
    assert_eq!(stats.accepted, 2);
    assert_eq!(stats.unknown_stream, 1);
    assert_eq!(stats.unauthenticated, 2);
    assert_eq!(stats.malformed, 3);
    assert_eq!(stats.rejected(), 6);

    demux.remove_stream(1);
    assert!(!demux.contains(1));
    assert_eq!(
        demux.open(&sealed(&key_a, 1, 2, b"")),
        Err(CoreError::UnknownStream(1))
    );
}

#[tokio::test]
async fn oversized_payload_is_refused_without_consuming_a_seq() {
    let hub = socket().await;
    let mut sender = MediaSender::new(socket().await, hub.local_addr().expect("addr"), 5);
    let err = sender
        .send(0, 0, &vec![0u8; MAX_MEDIA_PAYLOAD + 1])
        .expect_err("too large");
    assert!(matches!(
        err,
        CoreError::Proto(ProtoError::FrameTooLarge { .. })
    ));
    assert_eq!(sender.next_seq(), Some(0));
    assert!(sender
        .send(0, 0, &vec![1u8; MAX_MEDIA_PAYLOAD])
        .expect("max size"));
    let datagram = recv(&hub).await;
    assert_eq!(datagram.len(), hfa_proto::MAX_DATAGRAM);

    // A new destination (e.g. StreamAccepted.udp_port) keeps key and sequence.
    let other = socket().await;
    sender.set_dest(other.local_addr().expect("addr"));
    assert!(sender.send(0, 0, b"moved").expect("send"));
    let moved = recv(&other).await;
    let mut demux = MediaDemux::new();
    assert!(demux.add_stream(5, sender.key()));
    demux.open(&datagram).expect("first");
    let (header, payload) = demux.open(&moved).expect("second");
    assert_eq!(payload, b"moved");
    assert_eq!(header.seq, 1);
}
