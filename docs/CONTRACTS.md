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
core/                      Rust workspace (edition 2021, rust-version 1.85, resolver 2)
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
  - `MediaOpener::new(key, stream_id)` with `open(&self, datagram: &[u8]) -> Result<(MediaHeader, Vec<u8>)>`.
  - AEAD: ChaCha20-Poly1305. Nonce = `stream_id BE ‖ seq BE ‖ 0u32`. AAD = the 16-byte header. Each stream has a
    fresh key, and `seq` is never reused for a key.
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

## 6. `hfa-core` (networking + engines; tokio)

- `config.rs`: `Settings { device_name, port, bitrate, frame_ms, fec, jitter_min_ms, jitter_max_ms, output: OutputTarget, data_dir }`
  (serde JSON) with `load_or_default(dir)` and `save()`.
- `identity.rs`:
  - `Identity { keypair, device_id, name }`, loaded or created at `data_dir/identity.json` (unix mode 0600).
  - `TrustStore` (`trusted.json`) holding `TrustedPeer { device_id, name, public_key, paired_at }`, with `is_trusted`, `add` and `remove`.
- `pairing.rs`:
  - `PairingManager` (hub side) with `start(ttl) -> PairingInfo { pin, token, uri, expires_at_unix }`, `cancel()` and
    `verify_password(&str) -> bool`.
  - After 5 failed attempts the current pairing window is invalidated.
- `control.rs`:
  - `ControlChannel` over tokio `TcpStream`. The Noise XX handshake uses u16-length-prefixed messages; after it,
    Noise transport frames carry `encode_frame` payloads.
  - `connect(addr, &Identity, &TrustStore, pairing_secret: Option<String>) -> Result<(ControlChannel, PeerInfo)>`.
  - `accept(stream, &Identity, &TrustStore, &PairingManager) -> Result<(ControlChannel, PeerInfo)>`.
  - `send(&ControlMessage)` and `recv() -> ControlMessage`.
  - If the peer is unknown, pairing (SPAKE2 bound to the handshake hash) is mandatory, then the peer is added to the TrustStore.
- `discovery.rs`:
  - `Advertiser::start(name, device_id, port, platform)` for `_hfa._tcp` (TXT: `v`, `id`, `name`, `platform`).
  - `browse() -> Browser` yields `DiscoveryEvent { Found(HubInfo), Lost(device_id) }`.
  - `HubInfo { device_id, name, addrs, port, platform }`.
- `media.rs`: UDP media send/receive helpers built on `hfa-proto` sealing.
- `sender.rs`:
  - `SenderEngine::start(SenderConfig { hub: HubAddress, settings, capture: Box<dyn CaptureSource>, label, pairing_secret: Option<String> }) -> Result<SenderHandle>`.
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

## 7. `hfa-cli` (`hfa` binary, clap)

- `hfa hub [--port N] [--out default|device:<name>|wav:<path>|null] [--no-mdns] [--pair]`: prints the PIN and URI and shows a live sources table.
- `hfa send (--to host[:port] | --hub <name-or-id>) [--pin P | --uri hfa://…] [--source system|system-excl|tone:<hz>|wav:<path>|pid:<n>] [--bitrate] [--frame-ms]`
- `hfa discover`, `hfa devices` (outputs + capture apps + capabilities), `hfa trust list|remove <id>`.
- `hfa selftest [--seconds N] [--loss PCT] [--jitter MS]`: an in-process hub (WAV/Null output) plus a tone sender over
  localhost. It reports the measured latency, loss and glitches, and exits non-zero on failure. Used in CI.

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
