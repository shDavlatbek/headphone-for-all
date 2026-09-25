# Research: playing audio from many devices in one headphone

## 1. The problem: why a headphone can't mix two devices

| Technology | What it does | Why it doesn't solve the problem |
|---|---|---|
| **A2DP** (Bluetooth Classic stereo audio) | One source streams to one sink | The headphone decodes a single A2DP stream at a time. |
| **HFP/HSP** (calls) | Mono voice plus the mic | A separate profile. Headphones switch from A2DP to HFP for calls and don't mix them. |
| **Multipoint** | Headphone stays *connected* to 2 (rarely 3) devices | It **switches** the active audio source, for example pausing laptop music when a call rings on the phone. No mixing. |
| **Samsung "Dual Audio" and similar** | One phone → two headphones | The opposite problem (one source, many sinks). |
| **LE Audio / LC3** | New low-energy audio stack; several isochronous streams | Stream count and mixing support depend on the headphone and phone chipsets. Consumer headphones don't mix two independent phones. |
| **Auracast** (LE Audio broadcast) | One source broadcasts to unlimited receivers without pairing | Built for one-to-many. In theory each earbud can sync to a *different* broadcast, but that gives left = A and right = B, not a stereo mix, and support is rare. |

**Conclusion:** we can't change headphone firmware or the Bluetooth profiles. The mixing has to happen
*before* audio reaches the headphone, on a device the headphone is connected to.

## 2. Options we evaluated

### A. Software hub over Wi-Fi/LAN ✅ chosen

The headphone is paired to one device (the hub). Other devices capture their system audio, encode it
and stream it over the network to the hub, which mixes everything and plays the mix.

- ✅ Works with **any** headphone (Bluetooth, wired, USB).
- ✅ Any device can be the hub. No extra hardware.
- ✅ Adds low latency (about 30–80 ms on a good LAN) on top of the Bluetooth hop.
- ⚠️ Each sender needs our app, and each OS limits what can be captured (see §3).

### B. Bluetooth hardware "box" 🔜 future "Box mode"

A Raspberry Pi or mini-PC running Linux with BlueZ and PipeWire acts as an **A2DP sink for several phones
at once**. PipeWire mixes the streams automatically, and a second Bluetooth adapter sends the mix to
the headphone as an A2DP source.

- ✅ Needs **no app** on the phones. Works with iPhone, including DRM audio (Apple Music, Netflix), because the
  phone thinks it is playing to a speaker.
- ⚠️ Two Bluetooth hops, so about 150–300+ ms. Needs extra hardware and radio tuning (2 adapters, 2.4 GHz congestion).
- Worth doing later on top of the same Rust core (the box is just a Linux hub with Bluetooth inputs).

### C. LE Audio / Auracast ⏳ watch

Not reliable enough on consumer devices today to mix independent sources. We'll revisit once
Android and iOS expose broadcast-assistant APIs and headphones support it widely.

## 3. Capturing system audio on each OS

Every sender must capture "what the device is playing". This is the hardest platform-specific part.

| OS | API | Minimum version | Permission / UX | Limits |
|---|---|---|---|---|
| **Windows** | WASAPI **loopback** on the default render endpoint (whole system). **Process loopback** via `ActivateAudioInterfaceAsync` + `AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK` (include or exclude a process tree) | Loopback: Vista+. Process loopback: Windows 10 2004 in practice (documented as build 20348+) | None | Local speakers still play unless muted (see §9). Use *exclude* mode so the hub never captures itself. |
| **macOS** | **Core Audio process taps**: `AudioHardwareCreateProcessTap` + `CATapDescription` → aggregate device | macOS 14.2+ (14.4+ recommended) | "System audio recording" permission (`NSAudioCaptureUsageDescription`) | Per-app or global. The tap can **mute the original output** (`muteBehavior`), which is ideal for senders. Fallback: ScreenCaptureKit audio (macOS 13+, needs screen-recording permission). Older macOS would need a virtual driver like BlackHole (not planned). |
| **Linux** | **PipeWire** (or PulseAudio) *monitor* source of the default sink; per-app via PipeWire node links | Any modern distro | None | Easiest platform. PipeWire also ships RTP/ROC modules we can interoperate with. |
| **Android** | **`AudioPlaybackCapture`** (`AudioPlaybackCaptureConfiguration` + `AudioRecord`) using a **MediaProjection** token | Android 10 (API 29)+ | `RECORD_AUDIO` + a system "start recording/casting" consent dialog **each session** + a foreground service of type `mediaProjection` | Captures only `USAGE_MEDIA`, `USAGE_GAME` and `USAGE_UNKNOWN`. **Apps can opt out** (`ALLOW_CAPTURE_BY_NONE` / `allowAudioPlaybackCapture=false`); some streaming apps do. **Calls can't be captured.** |
| **iOS / iPadOS** | **No system-audio capture API.** Only route: a **ReplayKit Broadcast Upload Extension** receiving `RPSampleBufferType.audioApp` buffers | iOS 12+ (we target 15+) | The user starts the "broadcast" from Control Center or `RPSystemBroadcastPickerView` | Runs in a separate process with a **~50 MB memory cap**. Audio Units are not allowed inside the extension. **DRM-protected audio is silent** (Apple Music, Netflix and similar). A red status indicator shows while it runs. App Store review needs a clear purpose statement. |

**Hub side (playback):** no special API is needed. The hub opens a normal output stream to the default device
(the Bluetooth headphone), and the OS mixer combines our stream with the hub's own local audio.
- Android: play as `USAGE_MEDIA` **without taking exclusive audio focus**, so other apps keep playing.
- iOS: `AVAudioSession` category `.playback` with **`.mixWithOthers`**, plus the background-audio mode.

## 4. Codec

| Candidate | Verdict |
|---|---|
| **Opus** (RFC 6716) | ✅ **Chosen.** Low delay (2.5–60 ms frames), 48 kHz stereo, transparent music at 96–128 kbps, built-in **FEC and packet-loss concealment**, BSD licence, runs on every platform. |
| PCM (uncompressed) | Fallback or debug mode only: about 1.5 Mbit/s per stereo stream, fragile on Wi-Fi. |
| AAC / LC3 | No advantage for LAN transport. Licensing (AAC) or limited libraries (LC3). |

Defaults: 48 kHz, stereo, 10 ms frames (20 ms on weak Wi-Fi), 128 kbps, in-band FEC on.

## 5. Transport and real-time streaming

| Option | Pros | Cons | Use |
|---|---|---|---|
| **Custom RTP-like framing over UDP** | Minimal latency, full control, tiny code | We must build the jitter buffer, drift handling and encryption ourselves | ✅ **v0 transport** |
| **Roc Toolkit** (C library, MPL-2.0) | RTP + FECFRAME (Reed-Solomon, LDPC), **clock-drift compensation**, latency tuning, PipeWire modules, Android app | C dependency and cross-compiling for 5 OSes; no mixer of its own | 📚 Design reference; optional backend later; PipeWire interop |
| **QUIC datagrams** (`quinn`) | Encryption and congestion control included; control and media on one connection | Somewhat more overhead; jitter buffer still ours | Candidate for control channel / internet mode |
| **WebRTC** (libwebrtc, `webrtc-rs`) | NAT traversal, battle-tested jitter buffer and AEC | Heavy, complex to embed, tuned for voice | 🔜 Future "internet mode" only |

Required building blocks (whichever transport):
- **Adaptive jitter buffer:** absorbs Wi-Fi jitter. Target 20–60 ms, adapted to the jitter we measure.
- **Clock-drift compensation:** each device's sound card clock differs by tens of ppm, so streams slowly
  drift until the buffer runs dry or overflows. Fix: watch how full the buffer is and resample slightly, with a
  **variable-ratio resampler** (`rubato`) and a PI controller (Roc uses the same approach).
- **Loss handling:** Opus in-band FEC + PLC. Optionally send each packet twice on bad links.
- **Discovery:** mDNS/DNS-SD service `_hfa._udp`.
  - Android needs a `WifiManager.MulticastLock`.
  - iOS needs `NSLocalNetworkUsageDescription` + `NSBonjourServices` and should use native `NWBrowser`.
- **Pairing and encryption:**
  - The hub shows a **QR code** (address + public key + one-time token), or the user types a **6-digit PIN**.
  - The PIN uses a PAKE (`spake2`), so it can't be brute-forced offline.
  - Session keys come from a **Noise** handshake (`snow`); media is encrypted with ChaCha20-Poly1305.

## 6. Latency budget

| Stage | Typical |
|---|---|
| Capture buffer (sender) | 10–20 ms |
| Opus encode + packetize | ≈ 5 ms (plus the 10 ms frame) |
| Wi-Fi transit | 2–30 ms |
| Jitter buffer (hub) | 20–60 ms |
| Decode + mix + output buffer | 10–20 ms |
| **Subtotal (our system)** | **≈ 50–130 ms, target < 100 ms** |
| Bluetooth hop to the headphone (A2DP SBC/AAC; aptX LL, LC3 and "game mode" are lower) | 100–250 ms |

The Bluetooth hop happens whether or not our app runs, so for *listening* the extra delay is acceptable. The problem is
**video lip-sync on a sender device** (for example a movie on the tablet heard through the hub). That needs a hint in the UI and,
where possible, a "delay compensation" value the user can set in the video player. For reference, SonoBus measured about 22 ms
laptop → Android on Wi-Fi.

## 7. Cross-platform stack

| Option | Verdict |
|---|---|
| **Rust core + Flutter UI (`flutter_rust_bridge`)** | ✅ **Chosen.** One real-time-safe engine for all 5 OSes, one UI codebase, small native shims only where the OS forces it. |
| Kotlin Multiplatform + Compose | Great on Android; weak desktop and iOS low-level audio. |
| C++ + JUCE | Proven for real-time audio (SonoBus), but GPL or a commercial licence, and a dated mobile UI. |
| Rust + Tauri 2 | Good desktop story; mobile support newer; web UI adds overhead. |

Key Rust crates:
- `cpal`: cross-platform audio I/O, including WASAPI loopback on output devices, CoreAudio, ALSA/JACK and AAudio.
- `opus` / `audiopus`: libopus bindings.
- `rubato`: variable-ratio resampling.
- `rtrb` / `ringbuf`: lock-free SPSC buffers.
- `tokio`: async networking.
- `mdns-sd`: service discovery.
- `snow` (Noise) and `spake2`: pairing and encryption.
- `windows`: process loopback.
- `pipewire`: Linux capture.
- `objc2-core-audio`: macOS taps.
- `jni`: Android.

## 8. Prior art (what we reuse or learn)

| Project | What it is | Takeaway |
|---|---|---|
| [AudioRelay](https://audiorelay.net) | Commercial PC ↔ phone audio streaming | UX benchmark: device list, low latency. Closed source. |
| SoundWire | Older Windows/Linux → Android streaming | Shows demand; outdated latency and UX. |
| [SonoBus](https://www.sonobus.net) | Open-source (GPLv3) low-latency peer-to-peer audio, built on JUCE | Jitter-buffer and latency tuning ideas; about 22 ms on a LAN. |
| [Snapcast](https://github.com/badaix/snapcast) | Synchronized multi-room playback | Time sync (not our goal, since we want low latency and not sync). |
| [Roc Toolkit](https://roc-streaming.org) | Real-time audio-over-IP library with FEC and drift compensation, PipeWire modules | Main design reference for the transport; possible backend later. |
| [Scream](https://github.com/duncanthrax/scream) | Windows virtual sound card that streams over the LAN | Virtual-driver approach (if loopback without local playback is needed). |
| Airfoil (Rogue Amoeba) | Sends Mac audio to many receivers | Product reference for per-app source selection. |
| PipeWire BT speaker setups | Raspberry Pi as a multi-phone A2DP sink | Proves "Box mode" works. |

## 9. Open questions to confirm in prototypes

1. **Local playback on senders.** macOS taps can mute the original output. On Windows loopback and Android playback
   capture, does the device keep playing out loud? If muting the device also silences the capture, we need a
   workaround (for example Windows process loopback + per-app session volume, or an optional virtual device).
2. **Android:** which popular apps opt out of playback capture (Spotify, YouTube, Netflix…)? Keep a public
   compatibility list.
3. **iOS:** will App Store review accept an audio-only broadcast extension? Can the Rust sender and Opus fit
   within the 50 MB cap? (It should: an Opus encoder plus a socket is small.)
4. Real-world Bluetooth latency per codec, and whether the hub can pick a low-latency codec automatically.

## Sources

- Bluetooth multipoint: [SoundGuys: What is Bluetooth multipoint](https://www.soundguys.com/bluetooth-multipoint-explained-28601/), [Shokz: Multipoint vs Dual Audio](https://shokz.com/blogs/news/bluetooth-multipoint-vs-dual-audio), [Bose: Bluetooth multipoint](https://www.bose.com/stories/bluetooth-multipoint)
- LE Audio / Auracast: [Wikipedia: Auracast](https://en.wikipedia.org/wiki/Auracast), [Novel Bits: LE Audio & Auracast profile stack](https://novelbits.io/bluetooth-le-audio-auracast-profiles/)
- Windows: [MS Learn: Loopback recording](https://learn.microsoft.com/en-us/windows/win32/coreaudio/loopback-recording), [MS Learn: Application loopback sample](https://learn.microsoft.com/en-us/samples/microsoft/windows-classic-samples/applicationloopbackaudio-sample/), [MS Learn: AUDIOCLIENT_ACTIVATION_TYPE](https://learn.microsoft.com/en-us/windows/win32/api/audioclientactivationparams/ne-audioclientactivationparams-audioclient_activation_type)
- macOS: [Apple: Capturing system audio with Core Audio taps](https://developer.apple.com/documentation/CoreAudio/capturing-system-audio-with-core-audio-taps), [AudioCap sample (insidegui)](https://github.com/insidegui/AudioCap), [Recall.ai: Core Audio taps deep-dive](https://www.recall.ai/blog/core-audio-taps)
- Android: [Android Developers: Capture video and audio playback](https://developer.android.com/media/platform/av-capture), [Android Developers Blog: Capturing audio in Android Q](https://android-developers.googleblog.com/2019/07/capturing-audio-in-android-q.html)
- iOS: [Apple: ReplayKit security](https://support.apple.com/guide/security/replaykit-security-seca5fc039dd/web), [Fora Soft: ReplayKit broadcast extension (50 MB limit)](https://www.forasoft.com/blog/article/how-to-implement-screen-sharing-in-ios-1193)
- Transport: [Roc Toolkit features](https://roc-streaming.org/toolkit/docs/about_project/features.html), [Roc: frequency estimator and resampler](https://roc-streaming.org/toolkit/docs/internals/fe_resampler.html), [Opus codec](https://opus-codec.org/)
- Box mode: [Collabora: Raspberry Pi as a Bluetooth speaker with PipeWire](https://www.collabora.com/news-and-blog/blog/2022/09/02/using-a-raspberry-pi-as-a-bluetooth-speaker-with-pipewire-wireplumber/)
- Stack: [cpal](https://github.com/RustAudio/cpal), [flutter_rust_bridge](https://github.com/fzyzcjy/flutter_rust_bridge), [rubato](https://github.com/HEnquist/rubato), [Tauri](https://en.wikipedia.org/wiki/Tauri_(software_framework))
- Prior art: [SonoBus user guide](https://www.sonobus.net/sonobus_userguide.html), [AlternativeTo: AudioRelay alternatives](https://alternativeto.net/software/audiorelay/)
