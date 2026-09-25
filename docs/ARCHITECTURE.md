# Architecture

## 1. Roles

Every install of the app can act in two roles:

- **Sender:** captures this device's audio → encodes it → streams it to a hub.
- **Hub:** the device the headphone is connected to. It receives N streams → decodes → buffers → corrects
  drift → mixes → plays.

A device can also be a hub and a sender to a *different* hub, but never to itself (loop protection).
The hub's **own** local audio needs no capture: the OS mixer already sends it to the headphone, and our
hub adds one more output stream with the remote mix on top.

```
SENDER                                                   HUB
┌───────────────┐   ┌────────┐   ┌──────────┐            ┌──────────┐   ┌────────────┐   ┌──────────┐
│ OS capture    │──▶│ SPSC   │──▶│ Opus enc │──UDP──────▶│ recv +   │──▶│ per-stream │──▶│ decode + │
│ (loopback/    │   │ ring   │   │ + packet │            │ decrypt  │   │ jitter buf │   │ PLC/FEC  │
│  tap/…)       │   └────────┘   └──────────┘            └──────────┘   └────────────┘   └────┬─────┘
└───────────────┘                                                                            │
                                                         ┌──────────┐   ┌────────────┐   ┌────▼─────┐
                                        headphone 🎧 ◀───│ output   │◀──│ SPSC ring  │◀──│ drift    │
                                                         │ callback │   │            │   │ resample │
                                                         └──────────┘   └────────────┘   │ + MIXER  │
                                                                                         └──────────┘
          control channel (pairing, volume, mute, stats) ◀────────────────────────────▶
```

## 2. Tech stack

| Layer | Choice |
|---|---|
| Core engine | **Rust** workspace (shared by all platforms, compiled as `cdylib`/`staticlib`) |
| Audio I/O (playback, simple capture) | `cpal` (WASAPI, CoreAudio, ALSA/JACK, AAudio) |
| Codec | libopus through `opus` / `audiopus` |
| Resampling / drift | `rubato` (variable ratio) |
| Real-time buffers | `rtrb` (lock-free SPSC) |
| Networking | `tokio` + UDP (media); TCP or QUIC (`quinn`) for control |
| Serialization (control) | Protobuf (`prost`), so versions can evolve |
| Discovery | `mdns-sd` (desktop, Android); native `NWBrowser` on iOS |
| Security | `snow` (Noise XX / IK), `spake2` (PIN pairing), ChaCha20-Poly1305 for media |
| UI | **Flutter** (desktop + mobile) through **`flutter_rust_bridge` v2** |

### Platform capture backends

| OS | Where it lives | Implementation |
|---|---|---|
| Windows | Rust (`hfa-capture`) | `cpal` loopback for the whole system; `windows` crate for process loopback (exclude our own PID) |
| macOS | Rust (`hfa-capture`) | `objc2-core-audio`: `AudioHardwareCreateProcessTap` → aggregate device → IOProc |
| Linux | Rust (`hfa-capture`) | `pipewire` crate: capture stream linked to the default sink monitor |
| Android | Kotlin (`app/android`) | Foreground service + MediaProjection + `AudioRecord` with `AudioPlaybackCaptureConfiguration`; pushes PCM to Rust through JNI (`hfa-ffi`) |
| iOS | Swift (`app/ios/BroadcastExtension`) | ReplayKit `RPBroadcastSampleHandler` receives `.audioApp` buffers and calls the Rust **sender-only** static lib directly (no Flutter in the extension). It shares the paired-hub config with the app through an App Group. |

**Rule: audio never goes through Dart.** Flutter only drives UI and settings. PCM flows
native ↔ Rust through a C ABI, and every backend implements one trait:

```rust
pub trait CaptureSource: Send {
    fn format(&self) -> AudioFormat;             // sample rate, channels
    fn start(&mut self, sink: PcmSink) -> Result<()>; // sink = producer side of an SPSC ring
    fn stop(&mut self);
}
```

## 3. Wire protocol v0 (media over UDP)

One UDP datagram = one Opus frame (10 or 20 ms).

| Field | Size | Notes |
|---|---|---|
| `magic` | 2 B | `0x48 0x46` ("HF") |
| `version` | 1 B | `0` |
| `flags` | 1 B | bit0 FEC in payload, bit1 DTX/silence, bit2 stream reset |
| `stream_id` | 4 B | Random per stream; one sender can send several streams (per-app capture) |
| `seq` | 4 B | Packet counter, used for loss detection and as the AEAD nonce |
| `timestamp` | 4 B | In 48 kHz samples, used for jitter and drift estimation |
| `payload` | N B | Opus frame, encrypted with ChaCha20-Poly1305 (the header is authenticated as AAD) |

Bandwidth: about 140 kbit/s per stereo stream at 128 kbps Opus, which is trivial for Wi-Fi even with 10 senders.

### Control channel (per sender ↔ hub, encrypted with Noise)

`Hello{device_name, platform, app_version}` → `PairRequest{pake_msg | qr_token}` → `PairAccept{hub_pubkey}` →
`StreamStart{stream_id, codec, rate, channels, frame_ms, label}` / `StreamStop` →
`SetVolume{stream_id, gain}` / `Mute` / `SetPriority` (hub → sender, for the sender UI) →
`Ping/Pong` (RTT) → `Stats{loss, jitter, buffer_ms, latency_ms}` → `Bye`.

## 4. Hub engine

1. **Receiver task** (tokio): reads datagrams, authenticates and decrypts, then pushes packets into a per-stream
   **jitter buffer** ordered by `seq`.
2. **Mixer thread** (high priority, runs every 10 ms):
   - For each stream: pop a frame; decode it, or use Opus PLC/FEC if it is missing; then drift-resample.
     - A PI controller keeps the jitter buffer at its target level (the target adapts to measured jitter).
     - The resample ratio stays within ±0.2 %, which can't be heard.
   - Apply per-stream gain, mute and **ducking**.
   - Sum, run a **soft limiter**, and push to the output SPSC ring.
3. **Output callback** (`cpal`, real-time): only copies from the ring. No locks, no allocations, no syscalls.

### Mixer features

- Per-source volume and mute, plus a master volume.
- **Priority / ducking:**
  - The user marks a source as priority (for example "Work laptop – meetings").
  - When that source's level stays above a threshold, the other sources are lowered by about 12 dB, with attack and release smoothing.
- Soft limiter so several loud sources don't clip.
- A stream with no packets for 2 s is marked "idle". After 30 s it is removed, and the UI shows it as disconnected.

## 5. Sender engine

- The capture backend fills the SPSC ring.
- The encoder thread reads 10 ms frames, downmixes or resamples to 48 kHz stereo when needed, encodes Opus, and builds and encrypts packets.
- UDP send.
- The sender skips silent frames (DTX), which saves battery on mobile.
- Adaptive quality: when the hub reports loss, the sender raises Opus FEC and the expected loss %, then lowers the bitrate.

## 6. Repository layout

The exact module-level contract lives in [CONTRACTS.md](CONTRACTS.md).

```
headphone-for-all/
├── core/                         # Rust workspace
│   ├── hfa-proto/                # packet header, control messages (prost), crypto, pairing
│   ├── hfa-audio/                # Opus codec, jitter buffer, drift, resampler, mixer, meters
│   ├── hfa-core/                 # config, identity, discovery, control channel, sender + hub engines
│   ├── hfa-capture/              # CaptureSource backends: windows, macos, linux (cfg-gated)
│   ├── hfa-ffi/                  # C ABI + JNI + flutter_rust_bridge API surface
│   └── hfa-cli/                  # headless `hfa send` / `hfa hub` for testing & servers
├── app/                          # Flutter app (one UI for 5 OSes)
│   ├── lib/                      # Dart UI: device list, sources mixer, pairing (QR/PIN), settings
│   ├── android/                  # + Kotlin CaptureService (MediaProjection)
│   ├── ios/                      # + BroadcastExtension/ (Swift + Rust sender staticlib)
│   ├── macos/ windows/ linux/
├── docs/
└── .github/workflows/            # CI: cargo test/clippy, flutter analyze, per-OS builds
```

## 7. UI (Flutter)

- **Home:** "Be a hub" / "Send to a hub" toggle, and a list of discovered hubs.
- **Hub screen:** connected sources with a live level meter, volume slider, mute, priority star, stats
  (latency, loss). Plus a "Pair new device" button that shows a QR code and PIN.
- **Sender screen:** the target hub, a capture on/off button, per-app selection where the OS supports it (Windows, macOS),
  and a warning when the OS limits capture (Android opt-outs, iOS DRM).
- System tray / menu-bar mode on desktop; a persistent notification on Android (required by the foreground service).

## 8. Security and privacy

- **Pairing required.** Unknown devices can't inject audio into your headphone.
- All media and control traffic is encrypted; keys are pinned after the first pairing.
- LAN-only by default. Internet mode (relay/WebRTC) would be explicit and opt-in later.
- Always-visible "capturing" indicator on senders (the OS already forces this on Android and iOS).
- No audio is ever stored or sent anywhere except the paired hub.

## 9. Testing strategy

- Unit tests: packet (de)serialization, jitter buffer reorder/loss, drift controller convergence (simulated
  clocks at ±200 ppm), limiter.
- Loopback integration test: `hfa-cli send --tone 440` → `hfa-cli hub --out wav` on localhost, with simulated
  loss and jitter (`tc netem` on Linux). Check the output frequency and continuity.
- Latency measurement: a click track on the sender, and a mic or loopback on the hub, to measure glass-to-glass latency.
- Device matrix for manual QA: the Windows/macOS/Linux/Android/iOS × hub/sender combinations listed in the roadmap.
