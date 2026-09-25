# Implementation contract

This file is the **source of truth for how the code is split and how the parts talk to each other**.
Everyone working in parallel (humans or agents) codes against the names below. A signature can be
refined when there is a good reason (for example returning `Result`), but the **public names, module
locations and semantics must stay**. When you change a contract, update this file in the same commit.

## 0. Branches

- Integration branch: `claude/adoring-bohr-v3u309`, later merged into `main`.
- Each work package (WP) is developed on its own `feat/<wp>` branch, in its own git worktree
  at `.worktrees/<wp>/` (git-ignored). WPs are merged into the integration branch with `--no-ff`.
- Only edit files your WP owns (see §9). If you truly need a change elsewhere, keep it minimal and
  mention it in the commit message.

## 1. Repository layout

```
core/                      Rust workspace (edition 2021, rust-version 1.87, resolver 2)
  Cargo.toml               [workspace] + [workspace.dependencies] (all shared dep versions live here)
  hfa-proto/               wire format, control messages, crypto, pairing (pure: no tokio, no audio I/O)
  hfa-audio/               DSP + Opus codec: jitter buffer, drift, resampler, mixer, meters (no networking)
  hfa-capture/             OS audio I/O: capture backends + playback outputs
  hfa-core/                config, identity, discovery, control channel, pairing, sender + hub engines
  hfa-ffi/                 flutter_rust_bridge API + C ABI (iOS extension) + JNI (Android capture)
  hfa-cli/                 `hfa` binary (headless hub/sender, selftest)
app/                       Flutter app (Windows, macOS, Linux, Android, iOS)
docs/                      RESEARCH, ARCHITECTURE, ROADMAP, CONTRACTS, BUILDING
.github/workflows/         CI
```

## 2. Conventions (every Rust crate)

- Errors: one `thiserror` enum per crate (`ProtoError`, `AudioError`, `CaptureError`, `CoreError`). `anyhow` only in `hfa-cli`.
- Logging: `tracing`. Never log inside real-time code.
- No `unwrap()`/`expect()` in library code, except in tests or where it provably can't fail (add a comment saying why).
- **Real-time rules:**
  - The cpal/OS audio **callbacks** never allocate, lock, log or make syscalls. They talk to the rest of the program only
    through `rtrb` SPSC rings and atomics.
  - The hub **mixer thread** and sender **encoder thread** are "soft real-time": no blocking I/O and no
    unbounded allocation per tick. Short, uncontended `parking_lot::Mutex` sections are allowed.
- Audio samples are **interleaved `f32` in [-1, 1]**. The internal/network format is **48 kHz stereo**
  (`AudioFormat::INTERNAL`). Capture backends may deliver their native format; the sender converts it.
- Public items have doc comments. `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`
  and `cargo test --workspace` must pass on Linux. OS-specific code sits behind `#[cfg(target_os = "…")]`.
  Windows code must pass `cargo check --target x86_64-pc-windows-gnu`. macOS/iOS code is verified in CI.
- Speed up local builds with a shared target dir: `export CARGO_TARGET_DIR=/home/user/.cache/hfa-target`.

## 3. `hfa-proto` (pure, sans-IO)

`lib.rs` constants:

```rust
pub const PROTOCOL_VERSION: u8 = 0;
pub const DEFAULT_PORT: u16 = 47810;            // TCP control + UDP media use the same number
pub const SERVICE_TYPE: &str = "_hfa._tcp.local.";
pub const MAGIC: [u8; 2] = *b"HF";
pub const MEDIA_HEADER_LEN: usize = 16;
pub const AEAD_TAG_LEN: usize = 16;
pub const MAX_DATAGRAM: usize = 1400;
pub const MAX_CONTROL_FRAME: usize = 65_000;
```

- `media.rs`:
  - `MediaHeader { flags: u8, stream_id: u32, seq: u32, timestamp: u32 }` with `encode(&self) -> [u8; 16]` and
    `decode(buf: &[u8]) -> Result<(MediaHeader, &[u8]), ProtoError>`.
  - Byte layout: magic(2) version(1) flags(1) stream_id(4 BE) seq(4 BE) timestamp(4 BE).
  - Flags: `FLAG_FEC = 0x01`, `FLAG_DTX = 0x02` (silence keep-alive, empty payload), `FLAG_RESET = 0x04`.
- `crypto.rs`:
  - `MediaKey([u8; 32])` with `generate()`, `from_bytes`, `as_bytes`.
  - `MediaSealer::new(key, stream_id)` with `seal(&self, header: &MediaHeader, payload: &[u8], out: &mut Vec<u8>) -> Result<()>`.
    The output is `header ‖ ciphertext ‖ tag`.
  - `MediaOpener::new(key, stream_id)` with `open(&mut self, datagram: &[u8]) -> Result<(MediaHeader, Vec<u8>)>`
    (stateful: anti-replay window, see §3.1).
  - AEAD: ChaCha20-Poly1305. Nonce = `stream_id BE ‖ seq BE ‖ 0u32`. AAD = the 16-byte header. Each stream has a
    fresh key, and `seq` is never reused for a key: it is strictly increasing for the key's lifetime, never resets
    (not even with `FLAG_RESET`) and never wraps.
- `control.rs`:
  - A prost `ControlMessage { oneof body }` using hand-written `#[derive(prost::Message)]` structs (no protoc needed).
  - Variants:
    - `Hello{protocol_version, device_id, device_name, platform, app_version, role}` (role: Hub|Sender).
    - Pairing: `PairStart{method}` (Pin|Token), `PairSpake{msg}`, `PairConfirm{mac}`, `PairResult{ok, reason}`.
    - Streams: `StreamStart{stream_id, sample_rate, channels, frame_ms, bitrate, label, media_key}`,
      `StreamAccepted{stream_id, udp_port}`, `StreamRejected{stream_id, reason}`, `StreamStop{stream_id}`.
    - Controls: `SetVolume{stream_id, gain}`, `SetMute{stream_id, muted}`, `SetPriority{stream_id, priority}`.
    - Keep-alive and feedback: `Ping{nonce, t_us}`, `Pong{nonce, t_us}`,
      `Stats{stream_id, loss_pct, jitter_ms, buffer_ms, latency_ms, recommended_bitrate}`, `Bye{reason}`.
  - Helpers: `encode_frame(&ControlMessage) -> Result<Vec<u8>>` (u32 BE length prefix; see §3.2) and a streaming `FrameDecoder`
    (`push(&[u8])`, `next() -> Option<Result<ControlMessage>>`).
- `noise.rs`:
  - `NOISE_PATTERN = "Noise_XX_25519_ChaChaPoly_BLAKE2s"`.
  - `StaticKeypair { private: [u8;32], public: [u8;32] }` with `generate()`.
  - `NoiseHandshake::{initiator(&StaticKeypair), responder(&StaticKeypair)}` with `write_message(payload) -> Vec<u8>`,
    `read_message(&[u8]) -> Vec<u8>`, `is_finished()`, `remote_static() -> Option<[u8;32]>`,
    `handshake_hash() -> [u8;32]` and `into_transport() -> NoiseTransport`.
  - `NoiseTransport` has `encrypt(&mut self, &[u8]) -> Result<Vec<u8>>` and `decrypt`.
- `pairing.rs`:
  - SPAKE2 (`spake2` crate, Ed25519 group, symmetric), bound to the Noise handshake hash.
  - `PairingSession::start(password: &str, handshake_hash: &[u8;32]) -> (PairingSession, Vec<u8>)`.
  - `finish(self, peer_msg: &[u8]) -> Result<PairingKey>`.
  - `PairingKey::confirm_mac(role) -> [u8;32]` and `verify(role, mac)`. MACs are HMAC-SHA256 over role label ‖ handshake hash.
  - `generate_pin() -> String` (6 digits) and `generate_token() -> String` (16 random bytes, base64url).
- `uri.rs`:
  - `PairingUri { host, port, hub_id: [u8;32], token, name }` with `to_string()`/`FromStr`.
  - Format: `hfa://pair?v=0&h=<host>&p=<port>&id=<b64url pubkey>&t=<token>&n=<urlencoded name>`.
- `identity.rs`: `fingerprint(pubkey: &[u8;32]) -> String`. It is the first 8 bytes of SHA-256, hex, grouped as `ab12-cd34-ef56-7890`.
  This is the `device_id`.

### 3.1 Refinements made by `feat/scaffold` (the code in `core/hfa-proto` is authoritative)

- `error.rs` holds `ProtoError` (`Clone + PartialEq + Eq`, string payloads); `lib.rs` has `pub type Result<T>`.
  Variants: `Truncated{needed,got}`, `BadMagic`, `UnsupportedVersion(u8)`, `StreamMismatch{expected,got}`, `Crypto`,
  `InvalidKey`, `FrameTooLarge{len,max}`, `Decode`, `InvalidMessage`, `Noise`, `Pairing`, `InvalidUri`.
- `MediaHeader::has_flag(flag) -> bool` (implemented).
- `MediaKey`: `generate() -> MediaKey`, `from_bytes([u8;32])`, **new** `from_slice(&[u8]) -> Result<MediaKey>` (for
  `StreamStart.media_key`), `as_bytes() -> &[u8;32]`. Zeroized on drop; `Debug` is redacted.
- `MediaSealer::new(&MediaKey, stream_id)`, `MediaOpener::new(&MediaKey, stream_id)`, both with `stream_id()`.
  `seal`/`open` reject a header whose `stream_id` differs (`StreamMismatch`). `seal` clears `out` first.
- `control.rs`: prost enums `Role { Unspecified = 0, Hub = 1, Sender = 2 }` and
  `PairMethod { Unspecified = 0, Pin = 1, Token = 2 }` (proto3: 0 = unset → protocol error).
  **Final wire tags** — `ControlMessage.body` oneof: Hello=1, PairStart=2, PairSpake=3, PairConfirm=4, PairResult=5,
  StreamStart=10, StreamAccepted=11, StreamRejected=12, StreamStop=13, SetVolume=20, SetMute=21, SetPriority=22,
  Ping=30, Pong=31, Stats=32, Bye=40. Field tags inside each message follow the order listed in §3
  (1, 2, 3, … ; see the `.proto` equivalent in the `control.rs` module docs). Types: ids/ports/rates/bitrate `uint32`,
  `nonce`/`t_us` `uint64`, gains/percentages/ms `float`, keys/MACs/SPAKE messages `bytes`.
- The oneof enum is `control::control_message::Body` (re-exported as `control::Body`); `ControlMessage::new(Body)` and
  `From<Body> for ControlMessage` exist.
- `FrameDecoder` implements **`Iterator<Item = Result<ControlMessage>>`** instead of an inherent `next()` (clippy
  `should_implement_trait`); plus `new()`, `push(&[u8])`, `buffered()`. An empty `body` decodes as `InvalidMessage`.
- `noise.rs`: `StaticKeypair::generate() -> Result<StaticKeypair>`; `NoiseHandshake::{initiator, responder}(&StaticKeypair)
  -> Result<NoiseHandshake>`; `write_message(&[u8]) -> Result<Vec<u8>>`; `read_message(&[u8]) -> Result<Vec<u8>>`;
  `into_transport(self) -> Result<NoiseTransport>` (snow can fail on all of these). `StaticKeypair` is zeroized on drop,
  `Debug` redacts the private key.
- `pairing.rs`: the role type is **`PairingRole { Hub, Sender }`**; `PairingRole::label()` is part of the wire contract:
  `b"hfa-v0 pair-confirm hub"` / `b"hfa-v0 pair-confirm sender"`. `PairingKey::verify(role, &[u8]) -> Result<()>`
  (constant time, `Pairing` error on mismatch). A wrong password is detected by `verify`, not by `finish`.
- `uri.rs`: `PairingUri` implements `Display` (hence `to_string()`) and `FromStr<Err = ProtoError>`; `URI_SCHEME = "hfa"`.
- The crate is `#![forbid(unsafe_code)]`.
- **Sequence numbers and `FLAG_RESET` (review fix).** `seq` starts at 0 and is strictly increasing for the lifetime
  of a `(MediaKey, stream_id)`: it never resets and never wraps past `u32::MAX`. `FLAG_RESET` only signals a
  timestamp/codec-state discontinuity (the hub resets that stream's jitter buffer and decoder); it never resets
  `seq` or the replay window. Restarting at `seq = 0` requires a new stream: new `StreamStart`, new `stream_id`,
  new key.
- **Anti-replay (review fix).** New module `replay.rs`: `REPLAY_WINDOW = 128` and `ReplayWindow { new(), check(seq)
  -> bool, accept(seq) -> bool, highest() -> Option<u32> }` (implemented and tested; highest seq + 128-bit bitmap,
  RFC 6479 / WireGuard style). `MediaOpener` owns one; **`open` takes `&mut self`**: decode header → stream id →
  `check` → AEAD open → `accept` (only after authentication, so forged packets cannot move the window). Duplicates
  and packets older than the window fail with the new **`ProtoError::Replay { seq }`**.
- **Wire addition (review fix):** `Hello.pairing_required` (`bool`, tag **7**): set by the hub iff it does not trust
  the sender's key; senders send `false`. See §6.1 for the pairing procedure. **Refined by `feat/core-net` (§6.2):**
  the sender sets it iff it does not trust the hub's key, and the hub's value also covers that request.
- **Secrets never reach `Debug` (review fix):** `StreamStart`, `PairSpake` and `PairConfirm` use `#[prost(skip_debug)]`
  with manual `Debug` impls (`media_key`, SPAKE message and MAC print only their length). `PairingUri`'s `Debug`
  redacts the token; its `Display` (the URI) contains the token and must not be logged.

### 3.2 Refinements made by `feat/proto` (the code in `core/hfa-proto` is authoritative)

- **`encode_frame(&ControlMessage) -> Result<Vec<u8>>`** (was infallible): `InvalidMessage` for an empty `body`,
  `FrameTooLarge` if the protobuf payload exceeds `MAX_CONTROL_FRAME`. New `control::FRAME_PREFIX_LEN = 4` and
  `decode_message(&[u8]) -> Result<ControlMessage>` (one payload without prefix; re-exported at the crate root).
- `FrameDecoder`: an undecodable or body-less frame (including an unknown oneof variant from a newer peer) yields
  `Some(Err(Decode | InvalidMessage))` for that frame only, and decoding continues. A length prefix above
  `MAX_CONTROL_FRAME` yields `FrameTooLarge` **without allocating** and **poisons** the decoder (buffer dropped, later
  input ignored; the error is returned **once**, then `next()` returns `None`, so error-skipping loops end; check
  `is_poisoned()` / `poison_error()`): the caller closes the connection. `Debug` prints sizes only.
- Media: `MediaOpener::open` rejects datagrams shorter than header + tag (`Truncated{needed: 32}`) or longer than
  `MAX_DATAGRAM` (`FrameTooLarge`); extra `MediaOpener::highest_seq()`. `MediaSealer::seal` does not allocate when
  `out` has capacity (reuse one buffer per stream). `hfa-proto/Cargo.toml` enables `chacha20poly1305/zeroize`.
- Noise (wire contract): **prologue `NOISE_PROLOGUE = b"hfa-v0 control"`** on both sides; `NOISE_MAX_MESSAGE = 65535`,
  `NOISE_TAG_LEN = 16`, `NOISE_MAX_PLAINTEXT = 65519` (a const assertion guarantees a maximal control frame fits).
  Oversize inputs give `FrameTooLarge`, a transport message below 16 bytes `Truncated`, snow failures `Noise(..)`.
  A remote static key of small order is rejected with `InvalidKey` (libsodium's blocklist; its DH output would be
  predictable). The first XX message payload is not encrypted, so **handshake payloads should be empty** and `Hello`
  goes over the transport. `handshake_hash()` is final only once `is_finished()`. Extras: `NoiseHandshake::is_my_turn()`,
  **`NoiseTransport::handshake_hash()` and `NoiseTransport::remote_static() -> [u8;32]`** (kept from the handshake, so
  `hfa-core` can call `into_transport` first and still bind pairing). Transport messages must be decrypted in order;
  after a failed `decrypt` the channel must be closed. `Debug` impls never print keys.
- Pairing (wire contract): SPAKE2 symmetric mode with **`SPAKE2_IDENTITY = b"hfa-pairing-v0"`**, password = UTF-8 PIN or
  token. **`PairingKey = HKDF-SHA256(ikm = SPAKE2 key, salt = Noise handshake hash, info = PAIRING_KDF_INFO = b"hfa pairing v0")`**,
  `confirm_mac(role) = HMAC-SHA256(key, role.label() ‖ handshake_hash)`. `finish` refuses the peer message if it equals our
  own (reflection) or is malformed (`Pairing`). `generate_pin()` uses rejection sampling (exactly uniform); constants
  `PIN_DIGITS = 6`, `TOKEN_BYTES = 16`, `MAC_LEN = 32`. `PairingKey`/`PairingSession` `Debug` are redacted.
- URI (wire contract, see `uri.rs` module docs): values percent-encoded (all but ASCII alphanumerics and `-._~:`; `+` is
  literal); IPv6 hosts bracketed in the URI (`h=%5Bfe80::1%5D`) but stored **without** brackets in `PairingUri.host`, no
  zone ids; strict parsing (scheme `hfa` / host `pair` case-insensitive, no fragment, each parameter exactly once, `v=0`,
  port 1..=65535 digits only, `id` = exactly 32 bytes of unpadded base64url, token base64url alphabet ≤ `MAX_TOKEN_LEN` = 128,
  host name `[A-Za-z0-9._-]{1,253}`, name ≤ `MAX_NAME_LEN` = 256 bytes without control characters, total ≤ `MAX_URI_LEN` = 2048);
  unknown parameters are ignored, surrounding whitespace is trimmed. Constant `URI_PATH = "pair"`.
  **Build it with `PairingUri::new(host: &str, port, hub_id, token, name: &str) -> Result<PairingUri>`** (review fix):
  it validates host (IPv6 with or without brackets), port ≠ 0 and token (`InvalidUri`) and sanitizes the free-form name
  with `sanitize_name(&str) -> String` (re-exported; drops control characters, truncates on a char boundary to
  `MAX_NAME_LEN`), so `to_string()` always parses back. `Display` also sanitizes the name.
- Identity: extra `is_fingerprint(&str) -> bool` (re-exported) and `FINGERPRINT_LEN = 19`, e.g. to tell a device id from
  a name in `HubAddress::Discover`.

## 4. `hfa-audio` (pure DSP + codec)

- `format.rs`: `AudioFormat { sample_rate: u32, channels: u16 }`, `AudioFormat::INTERNAL` (48 000 Hz, 2 channels),
  `frames_for_ms(ms) -> usize`.
- `convert.rs`: `i16_to_f32`, `f32_to_i16`, `to_stereo(input, in_channels, out)` (mono duplicate, >2 channels folded down).
- `opus.rs`:
  - `OpusConfig { sample_rate, channels, bitrate, frame_ms, fec, expected_loss_pct, low_delay: bool }`.
  - `OpusEncoder::new(cfg)` with `encode(&mut self, pcm: &[f32], out: &mut [u8]) -> Result<usize>`, `set_bitrate`,
    `set_expected_loss`, `frame_samples()`.
  - `OpusDecoder::new(sample_rate, channels)` with `decode(&mut self, pkt, out) -> Result<usize /*frames*/>`,
    `decode_fec(&mut self, next_pkt, out)` and `conceal(&mut self, out)` (PLC).
- `jitter.rs`:
  - `JitterConfig { frame_ms, min_target_ms, max_target_ms, initial_target_ms, capacity }`.
  - `JitterBuffer::new(cfg)`.
  - `push(seq, timestamp, arrival_us: u64, payload: Vec<u8>) -> PushResult {Accepted, Duplicate, TooLate, Overflow}`.
  - `pop() -> Pop { Packet(Vec<u8>), Missing { next: Option<Vec<u8>> }, Underrun }`: one frame per call, in `seq` order.
    `next` is the following packet, if already buffered, so the caller can decode FEC from it.
  - `buffered_ms()`, `target_ms()` (adaptive: RFC 3550 interarrival jitter estimate, `clamp(frame + 3·jitter)`),
    `is_primed()`, `stats() -> JitterStats {received, lost, late, duplicate, jitter_ms}`, `reset()`.
- `drift.rs`: `DriftController::new(DriftConfig{kp, ki, max_ppm})` (default max 2000 ppm) with
  `update(buffered_ms, target_ms, dt_s) -> f64`. It returns the relative resample ratio, clamped to `1 ± max_ppm·1e-6`.
- `resample.rs`:
  - `StreamResampler::new(channels, in_rate, out_rate, chunk_frames)`, built on `rubato`, with relative ratio support.
  - `set_ratio_relative(f64)`.
  - `process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<()>`: interleaved, and `out` is appended to.
- `mixer.rs`:
  - `SourceId = u32`. `MixerConfig { channels, duck_db: -12.0, duck_threshold_db: -40.0, attack_ms: 10, release_ms: 300, limiter: true }`.
  - `Mixer::new(cfg, frame_frames)`.
  - Source controls: `add_source(id)`, `remove_source(id)`, `set_gain(id, f32)`, `set_muted(id, bool)`, `set_priority(id, bool)`,
    `set_master_gain(f32)`.
  - `mix(&mut self, inputs: &[(SourceId, &[f32])], out: &mut [f32])` and `levels() -> Vec<(SourceId, Level)>`.
  - Ducking: while any priority source is louder than the threshold, the others are lowered by `duck_db` with attack/release
    smoothing. A soft limiter runs on the master. Every gain change is ramped over one frame, so there are no clicks.
- `meter.rs`: `Level { peak_db, rms_db }` and `LevelMeter`.
- `tone.rs`: `SineGenerator::new(freq, amplitude, format)` with `fill(&mut [f32])`.
- `wav.rs`: thin helpers over `hound` (`WavWriter`, `read_wav`).

### 4.1 Refinements made by `feat/scaffold` (the code in `core/hfa-audio` is authoritative)

- `AudioError` (`error.rs`): `Opus{code, message}`, `InvalidConfig`, `BufferSize{expected, got}`, `Resample`, `Wav`.
- `AudioFormat::new(rate, ch)` (const), `frames_for_ms(&self, ms: u32) -> usize`, `samples_for_ms(&self, ms: u32)`,
  `Default` = `INTERNAL` (implemented).
- `convert`: `i16_to_f32(&[i16], &mut [f32])`, `f32_to_i16(&[f32], &mut [i16])` (convert `min(len)` samples);
  `to_stereo(input: &[f32], in_channels: u16, out: &mut Vec<f32>)` clears `out`, then writes `frames * 2` samples.
- `opus`: `MAX_OPUS_PACKET = 1275`. `OpusConfig { sample_rate: u32, channels: u16, bitrate: u32, frame_ms: u32, fec: bool,
  expected_loss_pct: u8, low_delay: bool }`, `Default` = 48 kHz / 2 ch / 128 kbit/s / 10 ms / FEC on / 5 % / `APPLICATION_AUDIO`.
  `OpusEncoder::new(cfg) -> Result<Self>`, `config()`, `encode(..) -> Result<usize>`, `set_bitrate(u32) -> Result<()>`,
  `set_expected_loss(u8) -> Result<()>`, **new** `set_fec(bool) -> Result<()>` (the sender adapts FEC),
  `frame_samples()` = samples **per channel**. `OpusDecoder::new(rate, ch) -> Result<Self>`, `decode/decode_fec/conceal
  -> Result<usize /*frames*/>`, **new** `reset()`. Encoder and decoder must be `Send` (compile-time asserted in `lib.rs`).
- `jitter`: `JitterConfig` field types `u32` ms + `capacity: usize`, `Default` = 10 / 20 / 150 / 40 ms / 64 packets.
  `buffered_ms()` and `target_ms()` return `f64` (they feed the drift controller); `config()`.
  `Pop::Missing.next` is a *copy* of packet `seq + 1`, which stays buffered.
- `drift`: `DriftConfig { kp: f64, ki: f64, max_ppm: f64 }` (`Default`: 1e-5, 1e-6, 2000). Sign convention: buffer above
  target → ratio < 1. Extra `ratio()` and `reset()`.
- `resample`: `StreamResampler::new(channels: u16, in_rate: u32, out_rate: u32, chunk_frames: usize) -> Result<Self>`,
  `set_ratio_relative(f64) -> Result<()>`, `process(&[f32], &mut Vec<f32>) -> Result<()>`, `reset()`, getters.
- `mixer`: **`MixerConfig` gains `sample_rate: u32`** (attack/release need it). Times are `f32` ms. Gains are linear and
  clamped to 0.0..=4.0. `mix` must not allocate. Extra `master_level() -> Level`, `config()`, `frame_frames()`.
- `meter`: `SILENCE_DB = -120.0`, `Level::SILENT` (= `Default`), `amplitude_to_db`, `db_to_amplitude`,
  `LevelMeter::{new, process(&[f32]) -> Level, level(), reset()}`.
- `tone`: `SineGenerator::new(freq: f32, amplitude: f32, format)`, `fill(&mut self, &mut [f32])`.
- `wav`: `WavWriter::{create(&Path, AudioFormat) -> Result<Self>, write(&[f32]) -> Result<()>, finalize(self) -> Result<()>}`,
  `read_wav(&Path) -> Result<(AudioFormat, Vec<f32>)>` (32-bit float files written; int/float read).

### 4.2 Refinements made by `feat/audio` (the code and module docs in `core/hfa-audio` are authoritative)

- `opus`: `frame_ms` must be one of 5, 10, 20, 40, 60 (2.5 ms is valid for libopus but not expressible in the
  integer field). Encoder: VBR, `ENCODER_COMPLEXITY` (10 on desktop, 7 on Android/iOS), `OPUS_SET_INBAND_FEC(1)` +
  `OPUS_SET_PACKET_LOSS_PERC(expected_loss_pct)` when `fec` (loss 0 otherwise). **In-band FEC is content
  dependent:** libopus only carries it (SILK LBRR) in SILK/hybrid packets. For music-like/stationary input it codes
  CELT at our bitrates *whatever the expected loss* (measured: LBRR only in the ~300 ms encoder start-up, at
  32–128 kbit/s and 5–30 % loss), so **raising `expected_loss_pct` does not buy FEC for music** — do not build a
  loss-adaptation loop on that premise; real protection for music needs application-level redundancy. Speech-like
  input gets hybrid + LBRR once the expected loss is ≳ 10 %. `decode_fec` on a packet without LBRR falls back to
  PLC — it always returns the requested frame count. **New** `packet_has_fec(&[u8]) -> bool` (LBRR present).
  **New** `OpusEncoder::lookahead(&mut self) -> Result<usize>` (codec delay in samples per channel, 312 at 48 kHz).
  `decode(&[])` (a `FLAG_DTX` keep-alive's empty payload) conceals one frame like `conceal` (libopus treats
  `len == 0` as a lost frame; after silent input the result is silence). `decode(&[])`/`decode_fec`/`conceal` need
  `out.len()/channels` to be a non-zero multiple of 2.5 ms (`BufferSize` otherwise). `set_bitrate`/`set_expected_loss` reject out-of-range
  values with `Opus{OPUS_BAD_ARG}` and leave `config()` unchanged. Encoder and decoder implement `Debug`.
- `jitter` (full semantics in the module docs):
  - **`PushResult::Overflow` now drops the *oldest* buffered slots** (the new packet is stored) when the buffer holds
    `capacity` packets or the packet would widen the window past `capacity` sequence numbers (a jump ahead);
    leading gaps are then skipped. (Before the first pop, a packet more than `capacity` behind is itself dropped.)
  - Before the first pop, packets older than the first buffered one are accepted (start-up reordering). After that,
    anything at or before the last played/skipped `seq` is `TooLate`.
  - When priming completes (buffered ≥ target), leading never-received slots are skipped, so playout (and re-priming
    after an `Underrun`) starts on a real packet instead of a run of `Missing`.
  - `JitterStats.lost` counts `Missing` frames plus slots skipped at priming or dropped by an overflow.
  - Target: rises immediately to `frame_ms + 3·J`, decays with time constant `TARGET_DECAY_MS = 8000` ms of pushed
    audio, clamped to `min..=max` and to `capacity · frame_ms`. Duplicates do not update `J`; late packets do.
  - `reset()` keeps the statistics **and the jitter estimate / target** (they describe the network). It also keeps
    a **sequence floor**: afterwards every packet at or before the highest `seq` ever pushed is `TooLate` (seq is
    never reused, so it predates the reset). **New** `reset_at(first_seq)`: same, but the floor is exactly
    `first_seq − 1` (pass the `FLAG_RESET` packet's seq), so a reordered pre-reset straggler is never played after
    the reset packet, while `first_seq` and later packets may still arrive in any order.
  - **New `Pop::Stretch`** (and `JitterStats.stretched`, `#[serde(default)]`): while primed, when `target_ms −
    buffered_ms > STRETCH_THRESHOLD_FRAMES (2) · frame_ms`, `pop()` asks for one synthesized frame *without*
    consuming a packet, at most once every `STRETCH_MIN_INTERVAL = 10` pops. This lets the buffer follow a risen
    target within about a second (the drift controller alone manages ≤ 2 ms/s).
- `drift`: **defaults are now `kp = 5e-5`, `ki = 1e-6`** (`max_ppm = 2000`): `ωn ≈ 0.032 rad/s`, `ζ ≈ 0.8`. The
  error is low-pass filtered (`MEASUREMENT_TAU_S = 2.0` s) before the PI law; anti-windup clamps the integral term to
  `±max_ppm` and freezes the integrator while saturated. Non-finite input or `dt_s <= 0` leaves the state untouched.
  **New** `ppm()` (ratio deviation in ppm). Ratio convention (also in `resample`): ratio = output frames per input
  frame multiplier; buffer too full → ratio < 1 → the consumer drains its input faster.
- `resample`: `rubato::Async` sinc (128 taps, Blackman-Harris², cubic), **fixed input chunk** of `chunk_frames`
  (output appears once a whole chunk is buffered: feed one 10 ms frame per call with `chunk_frames` = that frame for
  zero staging latency). Relative ratio range `1/MAX_RELATIVE_RATIO..=MAX_RELATIVE_RATIO` (`MAX_RELATIVE_RATIO =
  1.05`); ratio changes are ramped over one chunk. `reset()` keeps the relative ratio. **New** getters
  `ratio_relative()`, `pending_frames()`, `output_delay()` (filter delay in output frames). Verified no allocation in
  `process` after warm-up when `out` has capacity (`tests/no_alloc.rs`).
- `mixer`: new sources fade in from silence over their first block, and so does a source resuming after a tick in
  which it had no input slice (the hub leaves a not-yet-primed or underrunning stream out of `inputs`). With `limiter: false` the output is hard-clamped
  to [-1, 1] (never exceeds full scale either way; NaN/inf inputs become 0). The limiter is a stereo-linked peak
  limiter (instant attack, `LIMITER_RELEASE_MS = 80`, `LIMITER_THRESHOLD = 0.966`). The ducking detector uses the
  post-gain RMS of unmuted priority sources in the current block. `levels()` are after gain, mute and ducking. Inputs
  for unknown ids are ignored; short inputs are padded with silence. **New** `duck_gain()`, `source_ids()`,
  `MAX_GAIN = 4.0`.
- `meter`: `amplitude_to_db(0 | NaN) = SILENCE_DB`, `amplitude_to_db(inf) = f32::MAX` (always finite);
  `db_to_amplitude(<= SILENCE_DB) = 0`.
- `convert`: `f32_to_i16` scales by 32768 with rounding and saturation (exact inverse of `i16_to_f32`; NaN → 0).
  `to_stereo` fold-down for n > 2: `L = (c0 + w·Σc[2..]) / (1 + w·(n−2))`, same for R with c1, `w = 1/√2`.
- `wav`: `read_wav` also accepts 8-bit and 32-bit integer files.
- Workspace (`core/Cargo.toml`, scaffold-owned): `[profile.dev.package.rubato] opt-level = 3` so debug builds/tests
  (the 10-minute drift simulation) run the sinc resampler at full speed.
- **Hub per-stream recipe** (for `feat/core-engine`; exercised by `core/hfa-audio/tests/drift_sim.rs`): on receive,
  `jb.push(seq, timestamp, arrival_us, payload)`. Each mixer tick, while the stream's output FIFO holds less than one
  frame: `match jb.pop()` — `Packet(p)` → `dec.decode(&p)` (an empty `p` is a DTX keep-alive and is concealed, see
  `opus`); `Missing{next: Some(n)}` → `dec.decode_fec(&n)` (the next pop returns `n` itself, decoded normally);
  `Missing{next: None}` and `Stretch` → `dec.conceal()`; then `rs.process(pcm, &mut fifo)`; `Underrun` → no audio
  this tick (stop popping; leave the stream out of the mixer's `inputs` so it fades back in later). Take one frame
  from the FIFO for the mixer. Then, **while `jb.is_primed()`**, `let r = drift.update(jb.buffered_ms(),
  jb.target_ms(), frame_ms / 1000.0); rs.set_ratio_relative(r)`. On a `FLAG_RESET` packet with seq `R`:
  `jb.reset_at(R)` before pushing it, then `dec.reset()`, `rs.reset()`, `drift.reset()` (these belong to the
  mixer-side state; reset them before the first frame after the reset is decoded).

## 5. `hfa-capture` (OS audio I/O)

```rust
pub fn pcm_ring(capacity_samples: usize) -> (PcmSink, PcmSource);
impl PcmSink   { pub fn push(&mut self, interleaved: &[f32]) -> usize;  pub fn overruns(&self) -> u64; }
impl PcmSource { pub fn pull(&mut self, out: &mut [f32]) -> usize /* rest zero-filled */; pub fn available(&self) -> usize; pub fn underruns(&self) -> u64; }

pub trait CaptureSource: Send {
    fn describe(&self) -> String;
    fn format(&self) -> AudioFormat;
    fn start(&mut self, sink: PcmSink) -> Result<(), CaptureError>;
    fn stop(&mut self);
}
pub trait AudioOutput: Send {
    fn format(&self) -> AudioFormat;
    fn start(&mut self, source: PcmSource) -> Result<(), CaptureError>;
    fn stop(&mut self);
    fn latency_ms(&self) -> Option<f32>;
    fn has_error(&self) -> bool { false }   // added by feat/capture-common, see §5.3
    fn xruns(&self) -> u64 { 0 }            // added by feat/capture-common, see §5.3
}
pub enum CaptureTarget { SystemMix, SystemMixExcludingSelf, Process { pid: u32 }, Tone { freq_hz: f32 }, WavFile(PathBuf), External { id: u32 } }
pub struct CaptureApp { pub pid: u32, pub name: String }
pub struct Capabilities { pub system_mix: bool, pub per_app: bool, pub mutes_local_output: bool, pub notes: String }
pub fn capabilities() -> Capabilities;
pub fn list_capture_apps() -> Result<Vec<CaptureApp>, CaptureError>;       // Windows/macOS/Linux; else Ok(vec![])
pub fn open_capture(target: &CaptureTarget) -> Result<Box<dyn CaptureSource>, CaptureError>;
pub enum OutputTarget { Default, Device(String), WavFile(PathBuf), Null }
pub fn open_output(target: &OutputTarget, buffer_ms: u32) -> Result<Box<dyn AudioOutput>, CaptureError>;
pub fn list_output_devices() -> Result<Vec<String>, CaptureError>;
```

Modules:
- `ring.rs`
- `tone.rs` and `wav_source.rs`: real-time-paced threads.
- `external.rs`: a global registry, `register_external(id) -> ExternalFeed`. FFI pushes PCM with
  `ExternalFeed::push(&[f32], format)`. It is used by the Android and iOS native capture.
- `output_cpal.rs`
- `output_file.rs`: WAV and Null outputs, real-time paced.
- `linux.rs`: PipeWire monitor of the default sink (`stream.capture.sink=true`); `system-excl` and per-process capture
  link application playback streams into the capture stream (see §5.4).
- `windows.rs`: WASAPI loopback of the default render endpoint and process loopback, both directly via the
  `windows` crate (`AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`, include/exclude tree), plus session
  enumeration (see §5.2).
- `macos.rs`: Core Audio process tap (`AudioHardwareCreateProcessTap`, `CATapDescription`, private aggregate
  device, `muteBehavior = mutedWhenTapped`).

### 5.1 Refinements made by `feat/scaffold` (the code in `core/hfa-capture` is authoritative)

- `CaptureError` (`error.rs`): `Unsupported`, `PermissionDenied`, `NotFound`, `Format`, `AlreadyRunning`,
  `InvalidArgument`, `Backend`, `Io`, `Audio(#[from] AudioError)`; `From<io::Error>`. `lib.rs` has `pub type Result<T>`.
- `CaptureTarget` / `OutputTarget` implement `FromStr` + `Display` (implemented and tested) with the CLI string forms
  `system | system-excl | pid:<n> | tone:<hz> | wav:<path> | external:<id>` and `default | device:<name> | wav:<path> | null`.
  Both derive serde (`OutputTarget` is stored in `Settings`); `OutputTarget::default()` is `Default`.
- `open_capture` / `open_output` dispatch is implemented in `lib.rs` (wiring). WAV and null outputs use
  `AudioFormat::INTERNAL`; device outputs use the device's default config. `list_output_devices()` delegates to
  `output_cpal::list_devices()`.
- Ring semantics: `PcmSink::overruns()` / `PcmSource::underruns()` count **samples** dropped / zero-filled.
  Extra `PcmSink::{free, capacity}`, `PcmSource::capacity`.
- **`RingStats` (review fix):** `PcmSink::stats()` / `PcmSource::stats()` return a cloneable `RingStats`
  (`count() -> u64`, implemented) sharing the overrun/underrun counter. Take it **before** moving the ring end into
  `CaptureSource::start` / `AudioOutput::start`; it keeps counting afterwards (sender status, hub stats, selftest).
- Concrete types: `tone::ToneSource::new(freq_hz, format)`, `wav_source::WavFileSource::open(&Path) -> Result`,
  `external::ExternalSource::open(id) -> Result`, `output_cpal::CpalOutput::open(Option<&str>, buffer_ms) -> Result`,
  `output_file::WavFileOutput::create(&Path, AudioFormat, buffer_ms) -> Result`, `output_file::NullOutput::new(format, buffer_ms)`.
- **External feed API (refined):**
  ```rust
  pub fn register_external(id: u32, format: AudioFormat) -> ExternalFeed;   // registers or replaces feed `id`
  pub fn unregister_external(id: u32);
  #[derive(Clone)] pub struct ExternalFeed;                                  // Send + Sync (compile-time asserted)
  impl ExternalFeed { pub fn id(&self) -> u32; pub fn format(&self) -> AudioFormat;
                      pub fn push(&self, interleaved: &[f32]) -> usize; }    // any thread; 0 if no source is started
  ```
  The format is fixed at registration (so `ExternalSource::format()` is known before `start`). `push` takes a short
  `parking_lot` lock around the SPSC producer (native capture threads are not real-time callbacks). `push` does
  **not** convert: a caller whose format can vary per buffer (iOS ReplayKit) registers `AudioFormat::INTERNAL` and
  converts each buffer before pushing (see §8.1).
- **Platform-module interface.** `lib.rs` compiles exactly one of `linux.rs` (`target_os = "linux"`), `windows.rs`,
  `macos.rs`, or `unsupported.rs` (every other target, including Android and iOS) as `mod platform`. Each exposes exactly:
  ```rust
  pub(crate) fn capabilities() -> Capabilities;
  pub(crate) fn open_system(exclude_self: bool) -> Result<Box<dyn CaptureSource>, CaptureError>;
  pub(crate) fn open_process(pid: u32) -> Result<Box<dyn CaptureSource>, CaptureError>;
  pub(crate) fn list_apps() -> Result<Vec<CaptureApp>, CaptureError>;
  ```
  The OS stubs return `Err(Unsupported)`; `unsupported.rs` is final (`list_apps` → `Ok(vec![])`). Linux implements
  per-app capture too (§5.4). All OS dependencies are already declared in `hfa-capture/Cargo.toml`
  under `[target.'cfg(target_os = "…")'.dependencies]` (see §10), so OS work packages only edit their `src/<os>.rs`.
- macOS note: `objc2-core-audio` 0.3.2 (default features) already binds everything the tap backend needs:
  `AudioHardwareCreateProcessTap`/`AudioHardwareDestroyProcessTap`, `CATapDescription` (`initStereoGlobalTapButExcludeProcesses`,
  `initStereoMixdownOfProcesses`, `setPrivate`, `setMuteBehavior`, `UUID`), `CATapMuteBehavior`,
  `AudioHardwareCreateAggregateDevice` and `kAudioAggregateDeviceTapListKey`.
- **macOS backend (implemented by `feat/capture-macos`, `macos.rs` is authoritative):**
  - `AudioHardwareCreateProcessTap`/`AudioHardwareDestroyProcessTap` are resolved at run time with `dlsym`
    (a direct call would be a strong import that stops the binary from launching on macOS < 14.2); availability
    is gated by `objc2::available!(macos = 14.2)` + a `CATapDescription` class lookup. On older macOS every entry
    point returns `CaptureError::Unsupported` and `capabilities()` reports `system_mix = per_app =
    mutes_local_output = false` (all `true` on 14.2+).
  - Taps are private, get a fresh UUID and use `pub(crate) const MUTE_BEHAVIOR = CATapMuteBehavior::MutedWhenTapped`
    (the sender's own speakers go quiet while it streams). The tap and the private aggregate device are created by
    `open_*` (`format()` = the virtual format of the aggregate's first input stream, i.e. what the IOProc receives,
    which may differ from `kAudioTapPropertyFormat`), the IOProc by `start`; `stop`/`Drop` tear down in the
    order `AudioDeviceStop` → `AudioDeviceDestroyIOProcID` → `AudioHardwareDestroyAggregateDevice` →
    `AudioHardwareDestroyProcessTap`. `start` after `stop` rebuilds the tap (`Format` error if its format changed).
  - The aggregate device contains **only the tap** (no output sub-device, unlike AudioCap), so a headset
    microphone can never leak into the captured input buffers.
  - `list_apps` returns processes with `kAudioProcessPropertyIsRunningOutput`, excluding our own pid; the name is
    the last bundle-id component, else `proc_name`, sorted case-insensitively.
  - Errors: `'!hog'` → `PermissionDenied` (and `'nope'` only from `AudioHardwareCreateProcessTap`; elsewhere
    `'nope'` → `Backend`), `'unop'` → `Unsupported`, unknown or impossible (> `i32::MAX`) pid → `NotFound`, other
    statuses → `Backend`. `open_system(true)` fails with `Backend` rather than creating a non-excluding tap when this
    process has no Core Audio process object. `list_apps` also returns `Unsupported` before macOS 14.2. A denied
    permission may also just yield silence on some macOS versions.
  - **App requirement (`feat/apple`):** the macOS app's `Info.plist` must contain `NSAudioCaptureUsageDescription`.

### 5.2 Refinements made by `feat/capture-windows` (the code in `core/hfa-capture/src/windows.rs` is authoritative)

- **No cpal for capture on Windows.** System loopback (`open_system(false)`) and process loopback
  (`open_system(true)`, `open_process(pid)`) share one direct-WASAPI code path: `IAudioClient::Initialize(SHARED,
  LOOPBACK | EVENTCALLBACK | …)` + `IAudioCaptureClient`. This gives us `AUDCLNT_BUFFERFLAGS_SILENT` handling
  (pushed as zeros), device-invalidation recovery and identical threading for every mode. cpal is still used for
  playback (`output_cpal.rs`).
- **Format.** Every mode first asks for **32-bit float, 48 kHz, stereo** with `AUTOCONVERTPCM` (system loopback also
  `SRC_DEFAULT_QUALITY`), so capture normally already delivers `AudioFormat::INTERNAL`. Fallbacks: system loopback
  uses the endpoint mix format (8/16/24/32-bit PCM or 32-bit float, any channel count); process loopback uses 16-bit
  PCM 48 kHz stereo (the Microsoft sample's format). The backend **always delivers 2 channels** (mono duplicated;
  with more than two channels the front-left/right pair is kept) at the negotiated rate, which `format()` reports.
- **Threading.** `open_*` spawns one worker thread per source that enters the COM MTA, activates and initialises the
  client and reports the format back, so `format()` is valid and open errors (no device, unsupported OS) surface
  from `open_capture`, not from `start`. `start` hands the `PcmSink` to that thread; `stop` signals a stop event
  and joins. `start` after `stop` re-prepares the stream (it fails with `Format` if the format changed). COM
  pointers never cross threads except the activated client handed from the (MTA) activation callback.
- **Capture loop.** Waits on `[stop, audio]` events with a 20 ms timeout (some Windows builds never signal the
  event for loopback streams) and drains all packets; no allocation, locking or logging inside. On
  `AUDCLNT_E_DEVICE_INVALIDATED` / `AUDCLNT_E_SERVICE_NOT_RUNNING` (unplug, audio service restart) it re-opens the
  stream every 500 ms until it works or `stop` is called. System loopback also registers an `IMMNotificationClient`
  (Windows does not reroute a stream opened on a concrete `IMMDevice`): `OnDefaultDeviceChanged(eRender, eConsole)`
  signals a third event in the wait set and the stream is re-opened on the new default output. Other capture
  errors are re-opened after 500 ms, up to 5 times in a row without a delivered packet; a re-open that keeps
  landing on a different output format (10 tries) ends the worker. A `start` after the worker ended prepares a
  fresh one instead of returning `AlreadyRunning`. A pending process-loopback activation (max 10 s) watches the
  stop event, so `stop` never blocks on it; its activation parameters are owned by the completion handler, so
  they outlive an abandoned wait. The thread registers with MMCSS ("Pro Audio").
- **Errors.** Process-loopback activation failures map to `Unsupported` (message names Windows 10 build 20348,
  works from 19041), `E_ACCESSDENIED` to `PermissionDenied`; no default output device → `NotFound`;
  `open_process(0)` → `InvalidArgument`; a pid that does not exist → `NotFound`.
- **`list_apps`** runs on a short-lived MTA helper thread (never touches the caller's COM apartment), walks the
  sessions of **every active render endpoint** (a superset of the default one), skips system sounds, expired
  sessions, pid 0 and our own pid, names processes by executable stem (`Spotify`), falling back to the session
  display name or `Process <pid>`, and returns them deduplicated by pid and sorted by name.
- Verified: `cargo clippy --target x86_64-pc-windows-gnu --all-targets -D warnings`; the unit tests pass under wine 9
  on Linux, and a wine end-to-end run (PipeWire null sink + winepulse, with the endpoint switched to the capture
  device in a scratch copy because wine has no loopback) captured a 440 Hz tone at the expected level with no
  overruns, stopped in under 10 ms and restarted. Wine has no loopback or process loopback, so those two
  activations still need a real Windows 10/11 check.

### 5.3 Refinements made by `feat/capture-common` (the code in `core/hfa-capture` is authoritative)

- **Frame-aligned rings.** New `pcm_ring_with_channels(capacity_samples, channels: u16) -> (PcmSink, PcmSource)`
  (re-exported from `lib.rs`). Capacity is rounded down to whole frames (at least one frame; `channels == 0` → 1).
  `PcmSink::push` writes **whole frames only** (as many as fit) and returns the samples written; everything else
  (ring full, trailing partial frame) is dropped and counted in `overruns`. `PcmSource::pull` reads whole frames
  only and zero-fills the rest of `out` (counted in `underruns`). `pcm_ring(capacity)` is unchanged and equals
  `pcm_ring_with_channels(capacity, 1)` (sample-granular). New `PcmSink::channels()` / `PcmSource::channels()`.
  **Engines should create rings with `pcm_ring_with_channels(.., format.channels)`** of the capture/output format.
- **Output health and ring layout (review fix).** `AudioOutput` gained two default methods:
  `has_error() -> bool` (default `false`) and `xruns() -> u64` (default `0`). `has_error()` is `true` once the
  output failed while running and no longer plays/consumes the ring properly (cpal: device unplugged, stream
  invalidated; WAV: write error). It is reset by `start`. **The hub polls `has_error()` every mixer tick**; on
  `true` it stops the output and reopens it (or emits `HubEvent::Error`), since the ring no longer drains.
  Every output's `start` rejects a `PcmSource` whose `channels()` differs from `format().channels` with
  `InvalidArgument` (so the hub must build the output ring with `pcm_ring_with_channels(.., output.format().channels)`).
- Software sources and outputs (tone, WAV source, WAV/null outputs) run on a named thread paced against a
  monotonic clock (`Instant`), computed from the total frame count, so there is no cumulative drift; a thread that
  falls more than 250 ms behind skips ahead instead of bursting. `stop()` wakes and joins the thread; `Drop`
  stops. `start` on a running object → `AlreadyRunning`; all of them can be restarted after `stop`.
- `tone`: 10 ms blocks, amplitude `tone::TONE_AMPLITUDE = 0.25`, same signal on every channel.
- `wav_source`: `WavFileSource::open` reads the file with `hound` directly (8/16/24/32-bit int, 32-bit float) and
  delivers it in the file's own format, looping; `frames()`. Errors: missing file → `Io`, bad file → `Audio(Wav)`,
  no channels / no audio → `Format`. `hound` is now a direct dependency of `hfa-capture`.
- `external`:
  - At most **one** `ExternalSource` is attached to a feed at a time: a second `start` → `AlreadyRunning`, and
    `stop()`/drop of a source only detaches its own attachment.
  - `register_external(id, format)` with the **same format** as the registered feed returns a handle to that feed
    (a running source stays attached, e.g. when Android restarts its `AudioRecord`). With a **different format** it
    replaces the feed and detaches the old source; the sender must re-open the target.
  - `unregister_external` detaches the running source; unknown ids are ignored. `start` of a source whose feed was
    unregistered → `NotFound`.
  - New `ExternalFeed::is_attached()`; `ExternalFeed` implements `Debug`.
- `output_cpal::CpalOutput`:
  - Config choice: 48 kHz `f32` (stereo, else the fewest channels ≥ 2, else mono) → device default config →
    48 kHz in another supported sample format → any supported range at its max rate. Supported device sample
    formats: `f32`, `f64`, `i8`, `i16`, `i32`, `i64`, `u8`, `u16`, `u32`, `u64` (`f32` samples are clamped to
    [-1, 1] before conversion). `format()` reports the chosen rate/channels.
  - `buffer_ms` → `BufferSize::Fixed(frames)` clamped into the supported range (default buffer size when the range is
    unknown or `buffer_ms == 0`); if the fixed size is rejected, the default size is tried.
  - The stream lives on a dedicated thread (`cpal::Stream` may be `!Send`); `start` waits (≤ 10 s) until it plays.
    The default device is resolved again on `start`. The `PcmSource` reaches the callback through a one-slot
    `rtrb` hand-off ring. The data callback only pulls, converts and stores atomics.
  - cpal errors map to `NotFound` (device not available), `PermissionDenied`, `Format` (unsupported config) or
    `Backend`. Stream errors: `DeviceNotAvailable`, `StreamInvalidated`, backend errors... are fatal and set
    `has_error()` (inherent alias `has_stream_error()`). Not fatal: `Xrun` (counted, `xruns()`), `RealtimeDenied`,
    and `DeviceChanged` (the default-device stream was rerouted by cpal and keeps playing, e.g. a headphone became
    the default output; counted in the inherent `device_changes()`; the format is unchanged, nothing to reopen).
    Extra method: `sample_format()`.
  - `latency_ms()`: `None` until started; then the backend's callback→playback delay, else 2 × the device buffer.
- `output_file`: block period = `buffer_ms` clamped to 1..=100 ms; `latency_ms()` = that period. When the ring runs
  dry, the WAV output writes the zero-filled block (silence, like a device would play). `WavFileOutput::create`
  checks that the parent directory exists (`Io`) and the format is non-zero (`Format`); the file is created or
  truncated on `start` and finalized on `stop`/drop. Write errors are logged once, set `has_error()`, and the rest
  is discarded. The WAV `data` chunk is capped just below 4 GiB (`hound` keeps RIFF sizes in a `u32`; ~3.1 h at
  `AudioFormat::INTERNAL`): at the cap the file is finalized (whole frames), a warning is logged and the output
  keeps pulling and discarding at the same pace; that is not an error.

### 5.4 Refinements made by `feat/capture-linux` (the code in `core/hfa-capture/src/linux.rs` is authoritative)

- Every Linux source runs one thread (`hfa-pw-capture`) with its own PipeWire `MainLoop`/`Context`/`Core` and one
  input stream (`node.name`/`application.name` = `headphone-for-all`, `media.category=Capture`, `media.role=Music`)
  offering only F32LE interleaved 48 kHz stereo (PipeWire's adapter converts). The stream is connected in `open_*`,
  which waits (≤ 5 s) for the negotiated format, so `format()` is the negotiated one and a missing daemon is an
  `open_*` error (`CaptureError::Backend("cannot connect to the PipeWire daemon …")`), never a panic. Buffers
  captured before `start` are dropped. The `process` callback runs on the capture thread's loop (no `RT_PROCESS`)
  and only converts into a preallocated scratch buffer and pushes into the `PcmSink`.
- `SystemMix`: `stream.capture.sink=true` + autoconnect → monitor of the default sink (follows default-sink changes).
- `SystemMixExcludingSelf` is **implemented** (not a monitor fallback): the stream is left unconnected
  (`node.autoconnect=false`, `node.always-process=true` so silence flows while nothing is linked) and the backend
  links the output ports of every `Stream/Output/Audio` node whose process id ≠ ours into the stream's input ports
  (`link-factory`, `object.linger=false`), tracking streams as they come and go. Channel routing: same name, else
  left/right/centre onto FL/FR, LFE dropped.
- `Process { pid }`: the same linking restricted to the streams of `pid` **and its descendants** (`/proc/<pid>/stat`
  parent walk). Errors: `pid == 0` → `InvalidArgument`, no `/proc/<pid>` → `NotFound`; a process that is not playing
  yet is fine (its streams are linked when they appear).
- Relay streams are never linked or listed: `Stream/Output/Audio` nodes with `node.link-group` (loopback,
  filter-chain, echo-cancel, combine-stream halves) or `node.virtual=true`, and output streams of a client that also
  owns an `Audio/Sink` node (EasyEffects-style virtual sinks). The applications behind them are captured from their
  own streams, so nothing is captured twice and our own playback cannot come back through a virtual default sink.
  Sound reaching the sink only through a relay (e.g. a microphone loopback) is missed in the linked modes.
- A stream's pid: native clients → the kernel-verified `pipewire.sec.pid` (host namespace), else
  `application.process.id`; `pipewire-pulse` clients (`client.api`) → the node's, else the client's
  `application.process.id`. For sandboxed clients (`pipewire.access=flatpak` / `pipewire.access.portal.app_id`) a
  reported pid is namespace-local and is mapped to the host pid via `/proc/*/status` `NSpid` + the Flatpak app id
  (`/.flatpak-info` or the `app-flatpak-<id>-N.scope` cgroup); unmappable or ambiguous ones are not listed and not
  matched by `Process { pid }` (system-excl still links them). Nothing is decided before the node and client info
  have arrived.
- Links that fail (proxy error, link state `error`) or are removed by someone else are recreated, at most 3 times
  per link until its ports or our node change. Unwanted links are removed by dropping their proxy (one `destroy`).
  Only a broken connection (`-EPIPE`, `-ECONNRESET`, `-ENOTCONN`, `-ECONNREFUSED`, `-EPROTO` on the core) stops a
  capture; other core errors are logged at debug.
- `list_apps()` returns one `CaptureApp` per process with a playback stream (paused streams included), name =
  `application.name`, sorted by name; **this process is never listed**.
- `capabilities()` probes the daemon (a connect, no round trip): `system_mix = per_app = true` when reachable,
  both `false` otherwise (the reason is in `notes`); `mutes_local_output = false`.

## 6. `hfa-core` (networking + engines; tokio)

- `config.rs`: `Settings { device_name, port, bitrate, frame_ms, fec, jitter_min_ms, jitter_max_ms, output: OutputTarget, data_dir }`
  (serde JSON) with `load_or_default(dir)` and `save()`.
- `identity.rs`:
  - `Identity { keypair, device_id, name }`, loaded or created at `data_dir/identity.json` (unix mode 0600).
  - `TrustStore` (`trusted.json`) holding `TrustedPeer { device_id, name, public_key, paired_at }`, with `is_trusted`, `add` and `remove`.
- `pairing.rs`:
  - `PairingManager` (hub side) with `start(ttl) -> PairingInfo { pin, token, uri, expires_at_unix }`, `cancel()` and
    `begin_attempt(method) -> Option<PairingAttempt>` (see §6.1; replaces the earlier `verify_password`).
  - After 5 attempts without success the current pairing window is invalidated.
- `control.rs`:
  - `ControlChannel` over tokio `TcpStream`. The Noise XX handshake uses u16-length-prefixed messages; after it,
    Noise transport frames carry `encode_frame` payloads.
  - `connect(addr, &Identity, &TrustStore, expected_hub_key: Option<[u8;32]>, pairing_secret: Option<String>) -> Result<(ControlChannel, PeerInfo)>`.
  - `accept(stream, &Identity, &TrustStore, &PairingManager) -> Result<(ControlChannel, PeerInfo)>`.
  - `send(&ControlMessage)` and `recv() -> ControlMessage`.
  - If either side does not trust the other, pairing (SPAKE2 bound to the handshake hash) is mandatory, then both add
    each other to their TrustStore. A sender never streams to a hub it does not trust.
- `discovery.rs`:
  - `Advertiser::start(name, device_id, port, platform)` for `_hfa._tcp` (TXT: `v`, `id`, `name`, `platform`).
  - `browse() -> Browser` yields `DiscoveryEvent { Found(HubInfo), Lost(device_id) }`.
  - `HubInfo { device_id, name, addrs, port, platform }`.
- `media.rs`: UDP media send/receive helpers built on `hfa-proto` sealing.
- `sender.rs`:
  - `SenderEngine::start(SenderConfig { hub: HubAddress, settings, capture: Box<dyn CaptureSource>, label, expected_hub_key: Option<[u8;32]>, pairing_secret: Option<String> }) -> Result<SenderHandle>`.
  - Handle: `status() -> SenderStatus`, `events() -> broadcast::Receiver<SenderEvent>`, `stop()`.
  - `SenderStatus { state: Connecting|Pairing|Streaming|Reconnecting|Stopped|Failed(String), bitrate, loss_pct, rtt_ms, level_db }`.
  - Sends DTX keep-alives during silence. Adapts bitrate and FEC from `Stats`. Reconnects with backoff.
- `hub.rs`:
  - `HubEngine::start(HubConfig { settings, output: Box<dyn AudioOutput>, advertise: bool }) -> Result<HubHandle>`.
  - Source list: `sources() -> Vec<SourceInfo>`.
  - Source controls: `set_gain`, `set_muted`, `set_priority`, `set_master_gain`.
  - Pairing: `start_pairing() -> PairingInfo` and `cancel_pairing()`.
  - `events() -> broadcast::Receiver<HubEvent>`, `local_port()`, `stop()`.
  - `SourceInfo { stream_id, device_id, device_name, label, platform, gain, muted, priority, active, stats: StreamStats }`.
  - `StreamStats { loss_pct, jitter_ms, buffer_ms, latency_ms, level_db }`.
  - `HubEvent { SourceAdded, SourceRemoved, SourceUpdated, PairingCompleted{device_id, name}, PairingFailed{reason}, Error(String) }`.
  - The mixer thread is paced by the output ring fill level: it tops the ring up to the target latency every `frame_ms`.
  - It sends `Stats` to each sender every second. A stream is idle after 2 s without packets and removed after 30 s.
- `lib.rs` re-exports the engines and a `platform_name()` helper.

### 6.1 Refinements made by `feat/scaffold` (the code in `core/hfa-core` is authoritative)

- `CoreError` (`error.rs`, `Clone + PartialEq + Eq`): `Io`, `Json`, `Config`, `Proto(#[from])`, `Audio(#[from])`,
  `Capture(#[from])`, `Discovery`, `PairingRequired`, `PairingFailed`, `KeyMismatch`, `Protocol`, `Rejected`,
  `HubNotFound`, `Timeout`, `Closed`, `UnknownStream(u32)`; `From<io::Error>`, `From<serde_json::Error>`.
- `lib.rs`: `APP_VERSION`, `platform_name()` (implemented), re-exports; compile-time `Send + Sync` assertions for
  `HubHandle`, `SenderHandle`, `PairingManager`, `TrustStore` and `Send` for `SenderConfig`, `HubConfig`, `ControlChannel`.
- `config`: `SETTINGS_FILE = "settings.json"`; `Settings` is `#[serde(default)]`, `data_dir` is `#[serde(skip)]`.
  `Default` (implemented): port 47810, 128 kbit/s, 10 ms, FEC on, jitter 20..=150 ms, `OutputTarget::Default`,
  `default_device_name()`, `default_data_dir()` (`directories::ProjectDirs("io.github", "shdavlatbek", "headphone-for-all")`).
  **`default_device_name()` (review fix, implemented):** the OS host name (`libc::gethostname` on unix — `libc` is a
  `cfg(unix)` dependency of `hfa-core` — `COMPUTERNAME` on Windows), then the env vars `HOSTNAME`/`COMPUTERNAME`/`HOST`,
  then `hfa-XXXX` (4 random hex digits). `.local`/`.localdomain` suffixes are stripped and `localhost` is ignored
  (Android/iOS report it: the Flutter app sets `Settings.device_name` to the device model on mobile).
- `identity`: `IDENTITY_FILE`, `TRUST_FILE`. `Identity::load_or_create(data_dir, name) -> Result<Identity>`,
  `public_key()`. **`TrustStore` is a cheap-to-clone shared handle** (`Arc<Mutex<..>>`) whose methods take `&self`
  (so `accept(&TrustStore)` can add a newly paired peer): `load(data_dir) -> Result`, `is_trusted(&[u8;32]) -> bool`
  (by public key), `get(device_id) -> Option<TrustedPeer>`, `add(TrustedPeer) -> Result<()>`,
  `remove(device_id) -> Result<bool>`, `peers() -> Vec<TrustedPeer>`. `TrustedPeer.paired_at` is unix seconds.
- `pairing`: `PairingManager::new(hub_id: [u8;32], name: String, port: u16)`, `start(ttl) -> PairingInfo`, `cancel()`,
  `current()`. `DEFAULT_PAIRING_TTL = 300 s`. `method_for_secret(&str) -> PairMethod`: exactly 6 ASCII digits = PIN, else
  token (so `pairing_secret: Option<String>` stays untyped).
  **Guess budget (review fix, implemented and tested):** each SPAKE2 run is one online guess, so the secret is only
  handed out by **`begin_attempt(&self, PairMethod) -> Option<PairingAttempt<'_>>`**, which atomically (under the
  window lock) requires an open, unexpired window, **no other attempt in flight** and fewer than
  `MAX_FAILED_ATTEMPTS = 5` attempts so far, and **counts the attempt before SPAKE2 runs**. `PairingAttempt::secret()`
  is the PIN/token; `PairingAttempt::succeed(self)` closes the window (one-time secrets); dropping the guard otherwise
  (wrong MAC, error, disconnect, timeout) is a failure, and the window closes once 5 attempts were used. A second
  concurrent `PairStart` gets `PairResult{ok:false}`. Attempts are tied to a window generation, so a stale attempt
  never touches a newer window. The former `secret_for`, `verify_password`, `record_failure` and `record_success` are
  removed (an uncounted password check would bypass the budget). `PairingInfo`, `PairingManager` and
  `PairingAttempt` have redacting `Debug` impls.
- `control`: `connect`/`accept`/`send`/`recv` are `async`. After the handshake, every Noise transport message is also
  `u16` BE length-prefixed and carries one `encode_frame` payload. `PeerInfo { device_id, name, platform, app_version,
  public_key, addr: SocketAddr, newly_paired }`. `HANDSHAKE_TIMEOUT = 20 s`.
  **Procedure (review fix; the module docs of `hfa-core/src/control.rs` are authoritative):**
  1. Noise XX (sender = initiator). **`connect(addr, identity, trust, expected_hub_key: Option<[u8;32]>, pairing_secret)`**:
     if `expected_hub_key` is `Some` and differs from the hub's remote static key → `KeyMismatch` before anything else.
  2. The sender sends `Hello`; the hub answers with `Hello{pairing_required}` (= hub does not trust the sender).
  3. Pairing is needed iff the **sender does not trust the hub's key** or `pairing_required`. The sender-side check is
     mandatory whatever the hub signals, so a rogue hub cannot receive audio from an unpaired sender. Needed but no
     secret → sender sends `Bye`, `PairingRequired`. Otherwise `PairStart{method}` → `PairSpake` both ways → sender
     `PairConfirm` → hub verifies, answers `PairConfirm` + `PairResult`; both add each other to their `TrustStore`.
     The hub gets the secret only from `PairingManager::begin_attempt`. An untrusted sender whose next message is not
     `PairStart` gets `Bye` (`PairingRequired`).
  **Cancel safety (review fix):** `recv` **is cancel-safe** (it fills a private `rx: Vec<u8>` with
  `AsyncReadExt::read_buf` and only decrypts complete `u16`-prefixed records), so one task may run
  `tokio::select! { m = ch.recv() => .., _ = tick => ch.send(..).await, cmd = rx.recv() => ch.send(..).await }`.
  `send` is **not** cancel-safe: await it to completion inside a branch body, never as a branch future.
- `discovery`: `TXT_VERSION/TXT_ID/TXT_NAME/TXT_PLATFORM`; `Advertiser::start(..) -> Result<Advertiser>`, `stop(self)`;
  `browse() -> Result<Browser>`; `Browser::recv().await -> Option<DiscoveryEvent>` and `try_recv()` (not `next`, to avoid
  clashing with `Iterator`/`Stream`). `DiscoveryEvent::Lost(device_id)`.
- `media` (**review fix: nonce/replay safety**): **`MediaSender::new(Arc<UdpSocket>, dest, stream_id)` generates its own
  fresh `MediaKey`** (`key()` → put it in `StreamStart.media_key`; `stream_id()`), so a key is never reused with a
  restarted `seq`. Non-blocking `send(flags, timestamp, payload) -> Result<bool /*sent*/>` (usable from the encoder
  thread) fails with the new **`CoreError::SequenceExhausted(stream_id)`** after `seq = u32::MAX` (no wrap);
  `next_seq() -> Option<u32>`. To restart a stream either keep the sender and set `FLAG_RESET`, or create a new
  sender = new `stream_id` + key + `StreamStart`. `MediaDemux::{new, add_stream(..) -> bool, contains, remove_stream,
  open(&mut self, ..)}`: `add_stream` refuses (returns `false`) an id that is already registered (no hijacking, no
  replay-window reset); the hub answers that `StreamStart` with `StreamRejected`.
- `sender`: `HubAddress { Direct { host, port }, Discover { name_or_id } }`. **`SenderConfig.expected_hub_key:
  Option<[u8;32]>`** (review fix; from `PairingUri::hub_id` or a trusted peer) is passed to `connect`. `Discover` is
  unauthenticated mDNS: prefer trusted hubs when several match a name, and when `name_or_id` is a device id, fail with
  `KeyMismatch` unless `fingerprint(hub key) == name_or_id`. `SenderState` is a separate enum
  (`Connecting | Pairing | Streaming | Reconnecting | Stopped | Failed(String)`) used in `SenderStatus.state`.
  `SenderEvent { StateChanged(SenderState), Connected{device_id,name}, Paired{device_id,name}, HubControl{gain,muted,priority},
  Status(SenderStatus), Error(String) }`. **`SenderEngine::start(cfg).await`** (async; runs on the caller's tokio runtime,
  returns before the connection is up); `SenderHandle::stop(self).await`. `SenderConfig` has a manual `Debug` (redacts the secret).
- `hub`: `STATS_INTERVAL = 1 s`, `IDLE_AFTER = 2 s`, `REMOVE_AFTER = 30 s`. **`HubEngine::start(cfg).await`**;
  `settings.port = 0` binds any free port (see `local_port()`). `set_gain/set_muted/set_priority -> Result<()>`
  (`UnknownStream`), `set_master_gain(f32)`, `start_pairing() -> PairingInfo`, `cancel_pairing()`, `events()`, `local_port()`,
  **new** `device_id()`, `stop(self).await`. `HubEvent::{SourceAdded(SourceInfo), SourceRemoved{stream_id},
  SourceUpdated(SourceInfo), PairingCompleted{device_id,name}, PairingFailed{reason}, Error(String)}`.
  `StreamStats`/`SenderStatus` `Default` use `SILENCE_DB` for levels.

### 6.2 Refinements made by `feat/core-net` (the code and module docs in `core/hfa-core/src/{config,identity,pairing,control,discovery,media}.rs` are authoritative)

- **Control procedure (wire contract, `control.rs` module docs):**
  - `expected_hub_key` is compared right after Noise XX message 2 (where the initiator learns the hub's static key),
    so on `KeyMismatch(<hub device id>)` the sender aborts **before** message 3 and the hub never learns the sender's key.
  - **`Hello.pairing_required` from the sender** = "I do not trust your key" (it knows the hub key after the handshake).
    The hub answers `pairing_required = !hub_trusts_sender || sender_hello.pairing_required`. Both sides therefore agree
    whether a pairing phase follows without an extra round trip (otherwise a hub that trusts the sender could not tell
    whether the sender's next message is a `PairStart` or its first engine message). The sender's own trust check stays
    mandatory whatever the hub answers.
  - Both sides validate the peer `Hello`: `protocol_version == 0`, `role` (Sender ↔ Hub) and `device_id ==
    fingerprint(authenticated static key)` → `Protocol` (+ best-effort `Bye`). Names are sanitized
    (`hfa_proto::sanitize_name`; empty → the device id), `platform`/`app_version` truncated to 64 chars.
  - **Pairing message order:** sender `PairStart{method}` → hub `begin_attempt` → hub `PairSpake` (or
    `PairResult{ok:false, reason}` when `begin_attempt` returns `None`) → sender `PairSpake` + `PairConfirm` → hub
    verifies, **`attempt.succeed()` (now `-> bool`, see Pairing below; `false` → `PairResult{ok:false,"pairing was
    cancelled on the hub"}` + `PairingFailed`, nobody trusted)**, saves the sender in its `TrustStore`, then
    `PairConfirm` + `PairResult{ok:true}` → sender verifies the hub MAC and saves the hub. The sender waits for the hub's `PairSpake` before sending its own, so a
    refused `PairStart` never leaves unread bytes (a close with unread data becomes a TCP reset that can swallow the
    reason). Wrong secret → `PairResult{ok:false,"wrong PIN or token"}` and `PairingFailed` on both sides. A hub that
    fails to prove the secret → sender `Bye` + `PairingFailed`, nothing trusted. Invalid `PairStart.method` →
    `Protocol`. A sender `Bye` instead of `PairStart` → hub `PairingRequired`.
  - `HANDSHAKE_TIMEOUT` is one deadline around the whole phase (TCP connect included), so every step is bounded
    (`Timeout`) — **except the trust-store save after a successful pairing** (review fix): it starts only after the
    one-time secret was consumed and always completes (a `spawn_blocking` write cannot be cancelled, so a deadline in
    the middle used to leave the peer trusted on disk while the call reported `Timeout` and the secret stayed valid);
    the hub's `PairConfirm` + `PairResult` after it get the remaining time, at least 2 s. If those cannot be delivered
    the sender (which proved the secret) stays trusted on the hub and `accept` returns the error; the sender pairs
    again next time. Error mapping: I/O → `Io`, EOF → `Closed`, Noise/framing → `Proto`, unexpected message →
    `Protocol`. Trust-store writes run in `spawn_blocking` (any tokio runtime works).
  - `ControlChannel` extras: `close(self, reason).await` (best-effort `Bye` within 2 s + shutdown), `peer_addr()`,
    `remote_static()`, `handshake_hash()`, `is_closed()`; `RECORD_PREFIX_LEN = 2`. **Every error from `send`/`recv` is
    fatal** (later calls return it again or `Closed`), except an unencodable message in `send` (nothing written). A
    `send` future dropped mid-write marks the channel closed (next call → `Closed`). `recv` closes on a poisoned
    `FrameDecoder` (`Proto(FrameTooLarge)`). **Undecodable frames (review fix):** fatal (`Protocol`) during
    `connect`/`accept`; afterwards skipped (a newer peer's unknown variant), logged at most once per 5 s with a count,
    and more than **`pub const MAX_SKIPPED_FRAMES: u32 = 64`** in a row close the channel (`Protocol`). A transport
    record with an empty plaintext is fatal (`Protocol`). The record reader consumes records with an offset and
    compacts once per socket read (no per-record memmove). The single-task
    `tokio::select!` pattern (recv as branch future, `send` awaited in branch bodies) is a doc example in `control.rs`.
- **Pairing:** **`PairingAttempt::succeed(self) -> bool`** (`#[must_use]`, review fix): atomically checks that the
  attempt's window is still the current, unexpired one (not `cancel()`ed, not replaced by `start()`), and only then
  closes it and returns `true`; `false` means the pairing must be refused. `cancel()`/`start()` therefore really stop
  an attempt that is in flight. `PairingManager::with_clock(hub_id, name, port, UnixClock)` with `pub type UnixClock = Arc<dyn Fn() -> u64
  + Send + Sync>` (unix seconds; tests expire windows with it). `start(ttl)` rounds `ttl` up to whole seconds and builds
  the URI with `PairingUri::new(lan_ipv4() or 127.0.0.1, port, hub_id, token, name)`; if that fails (manager built with
  port 0) `uri` is `""` (warning logged) and the PIN still works — the hub engine must create the manager with its
  **bound** port. New `pub fn lan_ipv4() -> Option<Ipv4Addr>`: up, non-loopback, non-link-local, non-p2p IPv4,
  preferring RFC 1918 addresses and non-virtual interface names (docker/veth/vmnet/tun/wg/…).
- **Identity / trust files:** `identity.json` = `{"version":1,"private_key":"<b64>","public_key":"<b64>"}` (standard
  base64), created through a temp file opened with mode 0600 + `hard_link` (never overwrites; concurrent creators end
  up with the same key); a wider mode found on load is tightened to 0600; a corrupt file is an error (never regenerated).
  `trusted.json` = `{"version":1,"peers":[{device_id,name,public_key:"<b64>",paired_at}]}`, mode 0600, rewritten
  atomically (temp + rename); a failed save leaves the in-memory store unchanged. New `FILE_FORMAT_VERSION = 1`,
  `TrustedPeer::new(public_key, name)` (id = fingerprint, `paired_at` = now). `TrustStore::add` rejects a `device_id`
  that is not the key's fingerprint (`Config`); `load` drops such entries and rejects other file versions (`Config`).
  `TrustStore` has a manual `Debug` (path + count). **Readers never wait for disk I/O (review fix):** writers
  (`add`/`remove`) are serialized by a separate save lock held during write + fsync; the peer-list lock is held only
  to copy the list and to swap in the saved one, so `is_trusted`/`get`/`peers` are safe on async workers. `add` and
  `remove` still block — call them from `spawn_blocking` or a non-async thread.
- **Settings:** new `Settings::validate() -> Result<()>` (`Config`): device name non-blank, ≤ 256 bytes, no control
  characters; `frame_ms` ∈ `FRAME_MS_CHOICES = [10, 20]`; `bitrate` ∈ `MIN_BITRATE = 6000 ..= MAX_BITRATE = 510000`;
  `1 <= jitter_min_ms <= jitter_max_ms <= MAX_JITTER_MS = 2000`; every port (0 = any free port). `load_or_default`
  creates the directory, does not write defaults, and fails on invalid values (`Config`) or corrupt JSON (`Json`);
  `save` validates first and writes atomically (temp + fsync + rename).
- **Media:** `MediaSender` sends through a duplicated, non-blocking std handle of the tokio socket (fallback:
  `try_send_to`), so it never depends on reactor readiness (the first packet from the encoder thread goes out) and
  never blocks. **`send` returns `Ok(false)` (packet dropped, `seq` consumed) for transient errors (review fix):**
  `WouldBlock`, `ENOBUFS` (how macOS/iOS and some Wi-Fi drivers report a full queue; `WSAENOBUFS` on Windows),
  host/network unreachable or down, `EHOSTDOWN`, connection refused, interrupted. Any other `Err` is fatal for the
  stream. Extra `dest()`, `set_dest(addr)` (e.g. `StreamAccepted.udp_port`; key and seq continue),
  `MAX_MEDIA_PAYLOAD = MAX_DATAGRAM − 32` (larger payloads → `Proto(FrameTooLarge)`, no seq consumed). Announce each
  sender's key in exactly one `StreamStart`. `MediaDemux::open` rejects, cheapest first: bad header/size, unknown
  stream, **`seq` outside the stream's window** — more than **`MAX_SEQ_JUMP = 32768`** above the highest accepted seq
  (above 32768 before the first packet: streams start at 0) or duplicate/older than `REPLAY_WINDOW` (both
  `Proto(Replay{seq})`), then AEAD failure (`Proto(Crypto)`); the window moves only after authentication. Rejections
  are counted (`stats() -> DemuxStats {accepted, malformed, unknown_stream, out_of_window, unauthenticated}`,
  `rejected()`) and logged at most once per `REJECT_LOG_INTERVAL = 5 s`. Extras `len()`, `is_empty()`,
  `highest_seq(stream_id)`.
- **Discovery:** instance name `"<sanitized name> (<first 4 id chars>)"` (≤ 63 bytes, `.`/`\` replaced by `-`), host
  `hfa-<device id>.local.`, addresses follow the interfaces (`enable_addr_auto`), TXT `name` ≤ 200 bytes;
  `Advertiser::start` rejects port 0 / empty id; `Advertiser::fullname()`; `stop()`/`Drop` unregister (goodbye) and
  shut the daemon down without blocking. `browse()` runs a `hfa-mdns-browse` thread (mdns-sd's flume receiver →
  tokio mpsc, capacity 64, overflow dropped with a warning); only resolved instances with `v=0`, a valid fingerprint
  `id` and a non-zero port are reported (name falls back to the id, link-local IPv6 dropped, IPv4 first); `Lost(id)`
  when the last instance of that id is removed; dropping the `Browser` stops browsing and shuts the daemon down.
  Verified in the container: advertise + browse + goodbye over real multicast (`tests/net_discovery.rs`, not ignored).
- `hfa-core/Cargo.toml`: added `zeroize` (workspace dep) for wiping key material read from/written to disk.

## 7. `hfa-cli` (`hfa` binary, clap)

- `hfa hub [--port N] [--out default|device:<name>|wav:<path>|null] [--no-mdns] [--pair]`: prints the PIN and URI and shows a live sources table.
- `hfa send (--to host[:port] | --hub <name-or-id>) [--pin P | --uri hfa://…] [--source system|system-excl|tone:<hz>|wav:<path>|pid:<n>] [--bitrate] [--frame-ms]`
- `hfa discover`, `hfa devices` (outputs + capture apps + capabilities), `hfa trust list|remove <id>`.
- `hfa selftest [--seconds N] [--loss PCT] [--jitter MS]`: an in-process hub (WAV/Null output) plus a tone sender over
  localhost. It reports the measured latency, loss and glitches, and exits non-zero on failure. Used in CI.

### 7.1 Refinements made by `feat/scaffold`

- Global options: `--data-dir <DIR>` and `-v/--verbose` (count; `RUST_LOG` overrides).
- `send`: `--to` / `--hub` are mutually exclusive and **optional when `--uri` is given** (the URI carries host and port);
  `hfa send` with none of `--to`/`--hub`/`--uri` is rejected by clap (`required_unless_present_any` on `--to`,
  review fix); `--to`/`--hub` given together with `--uri` override the URI's host/port (the URI still supplies the
  hub key → `expected_hub_key`, and the token);
  `--pin` / `--uri` are mutually exclusive; `--pin` must be 6 digits; `--source` defaults to `system` and parses with
  `CaptureTarget::from_str`; `--bitrate` 6000..=510000; `--frame-ms` 10|20; extra `--label`.
  `--to` parses into `cli::HostPort { host, port }` (`host`, `host:port`, bare IPv6, `[ipv6]:port`; default port 47810).
- `hub --out` parses with `OutputTarget::from_str`. `discover --timeout <s>` (default 3).
  `selftest --seconds` 1..=3600 (default 5), `--loss` 0..=100 % (default 0), `--jitter` ms (default 0).
- Files: `src/main.rs` (runtime + tracing + dispatch), `src/cli.rs` (clap types, tested), `src/commands.rs` (bodies).

## 8. `hfa-ffi` + Flutter app

### 8.1 Rust FFI crate `core/hfa-ffi` (crate-type `cdylib`, `staticlib`, `rlib`)

A single global `EngineManager` (behind `OnceLock` + `parking_lot::Mutex`) owns one tokio multi-thread runtime,
the loaded `Settings`/`Identity`/`TrustStore`, and at most one `HubHandle` and one `SenderHandle`.

**flutter_rust_bridge v2 API** (`src/api/*.rs`; the Dart names are the lowerCamelCase versions generated into `app/lib/src/rust/`).
Every call returns a DTO; the engine types never cross the FFI boundary directly.

```rust
// api/app.rs
pub fn init_app(data_dir: String, device_name: Option<String>) -> anyhow::Result<AppInfo>;   // idempotent; sets up tracing
pub struct AppInfo { pub device_id: String, pub device_name: String, pub platform: String, pub version: String, pub capabilities: CapabilitiesDto }
pub struct CapabilitiesDto { pub system_mix: bool, pub per_app: bool, pub mutes_local_output: bool, pub external_only: bool, pub notes: String }
pub fn get_settings() -> anyhow::Result<SettingsDto>;
pub fn update_settings(settings: SettingsDto) -> anyhow::Result<()>;
pub struct SettingsDto { pub device_name: String, pub port: u16, pub bitrate: u32, pub frame_ms: u8, pub fec: bool, pub jitter_min_ms: u32, pub jitter_max_ms: u32, pub output_device: Option<String> }
pub fn list_output_devices() -> anyhow::Result<Vec<String>>;
pub fn trusted_peers() -> anyhow::Result<Vec<TrustedPeerDto>>;            // { device_id, name, paired_at_unix }
pub fn forget_peer(device_id: String) -> anyhow::Result<()>;
pub fn parse_pairing_uri(uri: String) -> anyhow::Result<PairingUriDto>;  // { host, port, hub_id, token, name }

// api/hub.rs
pub fn hub_start() -> anyhow::Result<HubStatusDto>;                      // { running, port, device_name, source_count }
pub fn hub_stop() -> anyhow::Result<()>;
pub fn hub_status() -> HubStatusDto;
pub fn hub_sources() -> Vec<SourceDto>;   // { stream_id, device_id, device_name, label, platform, gain, muted, priority, active, loss_pct, jitter_ms, buffer_ms, latency_ms, level_db }
pub fn hub_set_gain(stream_id: u32, gain: f32) -> anyhow::Result<()>;
pub fn hub_set_muted(stream_id: u32, muted: bool) -> anyhow::Result<()>;
pub fn hub_set_priority(stream_id: u32, priority: bool) -> anyhow::Result<()>;
pub fn hub_set_master_gain(gain: f32) -> anyhow::Result<()>;
pub fn hub_start_pairing() -> anyhow::Result<PairingInfoDto>;            // { pin, token, uri, expires_at_unix }
pub fn hub_cancel_pairing() -> anyhow::Result<()>;
pub fn hub_events(sink: StreamSink<HubEventDto>) -> anyhow::Result<()>;  // enum: SourceAdded(SourceDto) | SourceRemoved{stream_id} | SourceUpdated(SourceDto) | PairingCompleted{device_id, name} | PairingFailed{reason} | Error{message}

// api/sender.rs
pub fn discover_hubs(sink: StreamSink<DiscoveryEventDto>) -> anyhow::Result<()>; // enum: Found(HubInfoDto{device_id,name,addrs,port,platform}) | Lost{device_id}
pub fn stop_discovery() -> anyhow::Result<()>;
pub fn list_capture_apps() -> anyhow::Result<Vec<CaptureAppDto>>;       // { pid, name }
pub fn sender_start(request: SenderStartDto) -> anyhow::Result<()>;
pub struct SenderStartDto { pub hub_host: String, pub hub_port: u16, pub hub_device_id: Option<String>, pub hub_key: Option<String>, pub pairing_secret: Option<String>, pub source: CaptureSourceDto, pub label: String }
// hub_key = base64url static key from a pairing URI (-> SenderConfig.expected_hub_key); hub_device_id = fingerprint from discovery;
// when hub_key is None the FFI looks the key up in the TrustStore by hub_device_id (a trusted hub), else pairing must supply trust.
pub enum CaptureSourceDto { System, SystemExcludingSelf, Process { pid: u32 }, Tone { freq_hz: f32 }, External { feed_id: u32, sample_rate: u32, channels: u16 } }
pub fn sender_stop() -> anyhow::Result<()>;
pub fn sender_status() -> SenderStatusDto;  // { state: String ("idle"|"connecting"|"pairing"|"streaming"|"reconnecting"|"stopped"|"failed"), error: Option<String>, hub_name: Option<String>, bitrate, loss_pct, rtt_ms, level_db }
pub fn sender_events(sink: StreamSink<SenderStatusDto>) -> anyhow::Result<()>;
```

`CaptureSourceDto::External` registers `hfa_capture::register_external(feed_id, format)` before starting the sender.
Native code then pushes PCM into that feed.

**C ABI for the iOS broadcast extension** (`src/c_api.rs`, header `core/hfa-ffi/include/hfa_ext.h` checked in):

```c
typedef struct HfaExtSender HfaExtSender;
// config_json: {"data_dir": "...", "hub_host": "...", "hub_port": 47810, "hub_device_id": "...|null", "hub_key": "b64url|null", "label": "iPhone"}
HfaExtSender *hfa_ext_sender_start(const char *config_json);          // NULL on error
int32_t hfa_ext_push_pcm(HfaExtSender *h, const float *interleaved, uint32_t frames, uint32_t channels, uint32_t sample_rate); // 0 = ok
int32_t hfa_ext_sender_stop(HfaExtSender *h);                         // frees h; HFA_OK or error code (§8.4)
const char *hfa_ext_last_error(void);                                 // thread-local, valid until next call
```

The extension shares identity and trust with the app. `data_dir` is the App Group container, so the extension
reuses the pairing the app already did. Pairing never happens inside the extension.

**JNI for Android** (`src/android.rs`, `#[cfg(target_os = "android")]`). The class is `io.github.shdavlatbek.hfa.NativeBridge`:

```
external fun pushPcm(feedId: Int, data: FloatArray, frames: Int, channels: Int, sampleRate: Int): Int   // 0 = ok
external fun pushPcm16(feedId: Int, data: ShortArray, frames: Int, channels: Int, sampleRate: Int): Int
```

These are exported as `Java_io_github_shdavlatbek_hfa_NativeBridge_pushPcm` / `..._pushPcm16`. Kotlin calls
`System.loadLibrary("hfa_ffi")`; it is the same `.so` that flutter_rust_bridge loads.

### 8.2 Flutter app `app/`

- Package `headphone_for_all`, org `io.github.shdavlatbek`.
  - App ID `io.github.shdavlatbek.hfa` (Android applicationId, iOS/macOS bundle id).
  - iOS broadcast extension bundle id `io.github.shdavlatbek.hfa.broadcast`, App Group `group.io.github.shdavlatbek.hfa`.
- `app/flutter_rust_bridge.yaml`: `rust_input: crate::api`, `rust_root: ../core/hfa-ffi/`, `dart_output: lib/src/rust`.
  The native build uses frb's `rust_builder` (cargokit) at `app/rust_builder/`.
- State: `flutter_riverpod`. QR code: `qr_flutter` shows it and `mobile_scanner` scans it (Android/iOS only).
  Desktop tray: `tray_manager` + `window_manager`.
- Dart layout:
  - `lib/main.dart`
  - `lib/src/app.dart`
  - `lib/src/state/` (providers)
  - `lib/src/screens/` (home, hub, sender, pairing, settings, about)
  - `lib/src/widgets/`
  - `lib/src/platform/native_channel.dart` (wrapper for the channel below)
  - `lib/src/rust/` (generated; never edit by hand)

### 8.3 Platform channel `MethodChannel('hfa/platform')`

Dart side: `lib/src/platform/native_channel.dart`, owned by feat/app.
Native side: Android `MainActivity.kt` plus services (feat/android); iOS/macOS `AppDelegate.swift` (feat/apple).
Desktop Windows/Linux don't register the channel, so Dart must treat `MissingPluginException` as "not needed".

| Method | Args | Result | Android | iOS | macOS |
|---|---|---|---|---|---|
| `getDataDir` | – | `String` | `filesDir/hfa` | App Group container `/hfa` | Application Support `/hfa` |
| `startSystemCapture` | `{feedId:int, sampleRate:int, channels:int}` | `bool` (started) | MediaProjection consent → foreground `CaptureService` (type `mediaProjection`) → `AudioPlaybackCapture` → `NativeBridge.pushPcm` | returns `false` (use the broadcast picker) | not used (Rust captures directly) |
| `stopSystemCapture` | – | `null` | stops the service | – | – |
| `startHubService` | – | `null` | foreground `HubService` (type `mediaPlayback`) + keeps a `MulticastLock` | activates `AVAudioSession(.playback, .mixWithOthers)` | – |
| `stopHubService` | – | `null` | stops it, releases the lock | deactivates the session | – |
| `acquireMulticastLock` / `releaseMulticastLock` | – | `null` | `WifiManager.MulticastLock` | no-op | no-op |
| `writeBroadcastConfig` | `{hubHost, hubPort, hubDeviceId, hubKey, label}` | `null` | – | writes `broadcast_config.json` into the App Group container for the extension | – |
| `captureSupport` | – | `{supported:bool, reason:String}` | API ≥ 29 | `{supported:true, reason:"broadcast"}` | – |

iOS also registers the platform view `hfa/broadcast_picker`, which wraps an `RPSystemBroadcastPickerView`
whose `preferredExtension` is the broadcast extension's bundle id.

Events from native to Dart use `EventChannel('hfa/platform/events')` with maps `{type: "captureStopped"|"captureError"|"broadcastStarted"|"broadcastFinished", message?: String}`.

### 8.4 Refinements made by `feat/scaffold`

- C ABI (`c_api.rs`, `#[no_mangle] unsafe extern "C"`, never panics across the boundary):
  `hfa_ext_sender_start(config_json: *const c_char) -> *mut HfaExtSender` (null on failure),
  `hfa_ext_push_pcm(handle, samples: *const f32, frames: u32, channels: u32, rate: u32) -> i32`,
  `hfa_ext_sender_stop(handle) -> i32`. Codes: `HFA_OK = 0`, `HFA_ERR_INVALID_ARGUMENT = -1`, `HFA_ERR_CONFIG = -2`,
  `HFA_ERR_ENGINE = -3`, `HFA_ERR_NOT_IMPLEMENTED = -100`. `HfaExtSender` is opaque; the JSON schema is defined by `feat/ffi`.
- **PCM format of `hfa_ext_push_pcm` (review fix):** ReplayKit reveals the format per buffer, so the call carries it.
  The extension's `ExternalFeed` is registered as `AudioFormat::INTERNAL`; `hfa_ext_push_pcm` converts every buffer
  (`to_stereo`, plus a `StreamResampler` kept in `HfaExtSender` when `rate != 48000`) and re-creates the converter if
  rate/channels change mid-stream. `channels` outside 1..=8 or `rate` outside 8000..=192000 →
  `HFA_ERR_INVALID_ARGUMENT` (already checked in the stub). Android's `AudioRecord` format is fixed, so the JNI side
  registers the real format.
- `src/api/mod.rs` is a placeholder; `feat/ffi` adds `flutter_rust_bridge` to `hfa-ffi/Cargo.toml` itself.
- `src/android.rs` is compiled only for `target_os = "android"`; `jni` (0.22) is already an Android-only dependency.

## 9. Work packages and file ownership

| WP / branch | Owns |
|---|---|
| `feat/scaffold` | the workspace, every crate's `Cargo.toml`, `lib.rs` module wiring, stub signatures, `.gitignore`, licences, `rustfmt.toml` |
| `feat/proto` | `core/hfa-proto/**` |
| `feat/audio` | `core/hfa-audio/**` |
| `feat/capture-common` | `core/hfa-capture/**` except the `linux.rs`, `windows.rs`, `macos.rs` bodies |
| `feat/capture-linux` / `-windows` / `-macos` | the matching `core/hfa-capture/src/<os>.rs` (+ that OS's deps in `hfa-capture/Cargo.toml`) |
| `feat/core-net` | `core/hfa-core/src/{config,identity,pairing,control,discovery,media}.rs`, `hfa-core/Cargo.toml`, `hfa-core/tests/net_*.rs` |
| `feat/core-engine` | `core/hfa-core/src/{sender,hub,lib}.rs` (+ private helper modules it adds), `hfa-core/tests/engine_*.rs` |
| `feat/cli` | `core/hfa-cli/**`, the README "Try it" section |
| `feat/ffi` | `core/hfa-ffi/**`, creation of `app/` (`flutter create`), `app/flutter_rust_bridge.yaml`, `app/rust_builder/**`, `app/lib/src/rust/**` (generated), the minimal first `app/lib/main.dart`, app identifiers |
| `feat/app` | `app/lib/**` (except generated `lib/src/rust/**`), `app/pubspec.yaml`, `app/test/**`, `app/assets/**` |
| `feat/android` | `app/android/**` |
| `feat/apple` | `app/ios/**`, `app/macos/**` |
| `feat/desktop` | `app/windows/**`, `app/linux/**`, `packaging/**` |
| `feat/ci` | `.github/**`, `docs/BUILDING.md` |

## 10. Dependency choices (all versions live in `core/Cargo.toml` `[workspace.dependencies]`)

Latest stable releases as of 2026-09. Members use `dep = { workspace = true }`.

| Area | Crate (version) | Why / notes |
|---|---|---|
| Errors / logs | `thiserror` 2, `anyhow` 1 (cli only), `tracing` 0.1, `tracing-subscriber` 0.3 (`env-filter`, `fmt`) | |
| Serialization | `serde` 1 (`derive`), `serde_json` 1, `prost` 0.14 (hand-written derives, no `protoc`) | |
| Utilities | `parking_lot` 0.12, `once_cell` 1 (prefer `std::sync::OnceLock/LazyLock`), `directories` 6, `clap` 4 (`derive`), `rand` 0.10, `libc` 0.2 | |
| Crypto | `snow` 0.10, `spake2` 0.4 (Ed25519 group), `chacha20poly1305` 0.11, `sha2` 0.11, `hmac` 0.13, `hkdf` 0.13, `subtle` 2.6, `zeroize` 1.9 (`derive`), `base64` 0.23, `percent-encoding` 2.3 | `snow` is used **without default features** (`default-resolver`, `use-curve25519`, `use-chacha20poly1305`, `use-blake2`, `use-getrandom`): its `std` feature force-enables `ring`, which needs a C/asm toolchain per target. `spake2` 0.4 still uses `curve25519-dalek` 4 / `sha2` 0.10 internally — fine, the types never cross crate boundaries. |
| Audio | `rtrb` 0.4, `rubato` 5 (MSRV 1.87 → workspace `rust-version = "1.87"`), `hound` 3.5, `cpal` 0.18 | `rubato` 5 uses the `audioadapter` buffer API. |
| Opus | `opusic-sys` 0.7 | Pre-generated bindings (no bindgen); its `bundled` feature builds libopus 1.6.1 from source with CMake as a **static** library. Verified: Linux build+test, `x86_64-pc-windows-gnu` (mingw) link of `hfa.exe`, `aarch64-linux-android` link of `libhfa_ffi.so` (uses `ANDROID_NDK_HOME`'s CMake toolchain). Chosen over `audiopus_sys` (last release 2021). The safe wrapper is our own `hfa_audio::opus`. |
| Networking | `tokio` 1 (`rt-multi-thread`, `net`, `sync`, `time`, `macros`, `io-util`; tests add `test-util`), `mdns-sd` 0.21, `if-addrs` 0.15 (LAN address for the pairing URI) | |
| Windows | `windows` 0.62 + `windows-core` 0.62 | Features: `std`, `Win32_Foundation`, `Win32_Security`, `Win32_Devices_FunctionDiscovery`, `Win32_Devices_Properties`, `Win32_Media_Audio`, `Win32_Media_Audio_Endpoints`, `Win32_Media_KernelStreaming`, `Win32_Media_Multimedia`, `Win32_System_Com`, `Win32_System_Com_StructuredStorage`, `Win32_System_Variant`, `Win32_System_Threading`, `Win32_System_ProcessStatus`, `Win32_System_Diagnostics_ToolHelp`, `Win32_System_SystemServices`, `Win32_UI_Shell_PropertiesSystem`. Verified to resolve `ActivateAudioInterfaceAsync`, `AUDIOCLIENT_ACTIVATION_PARAMS`, `AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS`, `VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK`, `IAudioSessionManager2/IAudioSessionControl2`, `PROPVARIANT`, `CreateEventW`, `QueryFullProcessImageNameW`, ToolHelp snapshots and `#[implement(IActivateAudioInterfaceCompletionHandler)]`. Same major as cpal's. `windows-core` must stay on 0.62 (not the newest 0.100) to match `windows`. |
| Linux | `pipewire` 0.10 (+ `libc`) | Same major as cpal's optional PipeWire backend; SPA types via `pipewire::spa`. Needs `libpipewire-0.3-dev`, `libspa-0.2-dev`, `clang`. |
| macOS | `objc2` 0.6, `objc2-foundation` 0.3, `objc2-core-audio` 0.3, `objc2-core-audio-types` 0.3, `objc2-core-foundation` 0.3, `block2` 0.6 (+ `libc`) | One mutually compatible family (the same one cpal 0.18 uses). Default features (all framework bindings). |
| Android | `jni` 0.22 (hfa-ffi, Android only) | Same major as cpal's Android backend. |
| Dev | `proptest` 1.11, `tempfile` 3 | |

## 11. Cargo features and cross-target checks

- `bundled-opus` (default on every crate that links libopus: `hfa-audio`, `hfa-capture`, `hfa-core`, `hfa-ffi`, `hfa-cli`)
  forwards to `opusic-sys/bundled`. The workspace declares `hfa-audio`, `hfa-capture` and `hfa-core` with
  `default-features = false`, and each member re-enables them through its own `bundled-opus`, so normal builds always
  bundle libopus while `--no-default-features` links the system libopus instead and **skips the C build**.
- Apple targets cannot build libopus on the Linux dev container (no macOS SDK), so check Apple code with:
  `cargo check --workspace --all-targets --target aarch64-apple-darwin --no-default-features` (also `x86_64-apple-darwin`,
  `aarch64-apple-ios`). Real Apple builds (with bundled libopus) run in CI on macOS.
- Android: `cargo clippy --workspace --all-targets --target aarch64-linux-android` works with
  `ANDROID_NDK_HOME=/opt/android-sdk/ndk/<ver>`, `ANDROID_PLATFORM=android-24`,
  `CC_aarch64_linux_android`/`CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER` = `<ndk>/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android24-clang`
  and `AR_aarch64_linux_android` = `…/llvm-ar` (or simply `cargo ndk`).
