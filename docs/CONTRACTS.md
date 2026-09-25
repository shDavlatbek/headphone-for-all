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
  - Helpers: `encode_frame(&ControlMessage) -> Vec<u8>` (u32 BE length prefix) and a streaming `FrameDecoder`
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
  the sender's key; senders send `false`. See §6.1 for the pairing procedure.
- **Secrets never reach `Debug` (review fix):** `StreamStart`, `PairSpake` and `PairConfirm` use `#[prost(skip_debug)]`
  with manual `Debug` impls (`media_key`, SPAKE message and MAC print only their length). `PairingUri`'s `Debug`
  redacts the token; its `Display` (the URI) contains the token and must not be logged.

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
}
pub enum CaptureTarget { SystemMix, SystemMixExcludingSelf, Process { pid: u32 }, Tone { freq_hz: f32 }, WavFile(PathBuf), External { id: u32 } }
pub struct CaptureApp { pub pid: u32, pub name: String }
pub struct Capabilities { pub system_mix: bool, pub per_app: bool, pub mutes_local_output: bool, pub notes: String }
pub fn capabilities() -> Capabilities;
pub fn list_capture_apps() -> Result<Vec<CaptureApp>, CaptureError>;       // Windows/macOS; else Ok(vec![])
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
- `linux.rs`: PipeWire monitor of the default sink (`stream.capture.sink=true`).
- `windows.rs`: WASAPI loopback via cpal, plus process loopback via the `windows` crate
  (`AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`, include/exclude tree), plus session enumeration.
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
  The OS stubs return `Err(Unsupported)`; `unsupported.rs` is final (`list_apps` → `Ok(vec![])`). Linux should return
  `Ok(vec![])` from `list_apps` once implemented. All OS dependencies are already declared in `hfa-capture/Cargo.toml`
  under `[target.'cfg(target_os = "…")'.dependencies]` (see §10), so OS work packages only edit their `src/<os>.rs`.
- macOS note: `objc2-core-audio` 0.3.2 (default features) already binds everything the tap backend needs:
  `AudioHardwareCreateProcessTap`/`AudioHardwareDestroyProcessTap`, `CATapDescription` (`initStereoGlobalTapButExcludeProcesses`,
  `initStereoMixdownOfProcesses`, `setPrivate`, `setMuteBehavior`, `UUID`), `CATapMuteBehavior`,
  `AudioHardwareCreateAggregateDevice` and `kAudioAggregateDeviceTapListKey`.

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

- `hfa-ffi` (crate-type `cdylib`, `staticlib`, `rlib`):
  - `src/api/` is the flutter_rust_bridge v2 API. One global engine manager owns a tokio runtime.
  - `src/c_api.rs` is the C ABI for the iOS broadcast extension: `hfa_ext_sender_start(config_json)`,
    `hfa_ext_push_pcm(handle, *const f32, frames, channels, rate)`, `hfa_ext_sender_stop(handle)`.
  - `src/android.rs` holds the JNI exports for the Kotlin capture service. It pushes PCM into `ExternalFeed`.
- Flutter app `app/`:
  - Package `headphone_for_all`, org `io.github.shdavlatbek`.
  - App ID `io.github.shdavlatbek.hfa`, App Group `group.io.github.shdavlatbek.hfa`, broadcast extension
    `io.github.shdavlatbek.hfa.broadcast`.
  - The frb config `app/flutter_rust_bridge.yaml` points `rust_root` at `../core/hfa-ffi`. Dart output goes to `app/lib/src/rust/`.
  - Riverpod for state. `qr_flutter` shows the QR code, and `mobile_scanner` scans it on mobile.

### 8.1 Refinements made by `feat/scaffold`

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
| `feat/capture` | `core/hfa-capture/**` except `linux.rs`, `windows.rs`, `macos.rs` bodies |
| `feat/capture-linux` / `-windows` / `-macos` | the matching `core/hfa-capture/src/<os>.rs` (+ that OS's deps in `hfa-capture/Cargo.toml`) |
| `feat/core-engine` | `core/hfa-core/**` |
| `feat/cli` | `core/hfa-cli/**` |
| `feat/ffi` | `core/hfa-ffi/**` |
| `feat/app` | `app/lib/**`, `app/pubspec.yaml`, `app/test/**`, frb config |
| `feat/android` | `app/android/**` |
| `feat/apple` | `app/ios/**`, `app/macos/**` |
| `feat/desktop` | `app/windows/**`, `app/linux/**`, packaging |
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
