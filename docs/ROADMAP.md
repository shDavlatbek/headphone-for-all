# Roadmap

Goal of the MVP: headphone on **any** device. Windows, macOS, Linux, Android and iOS can all send their audio to it
and be heard **at the same time**.

Order of work: build the risky core first (transport, drift, mixing) on the easiest OSes, then add
platforms one at a time, and do the mobile hub last.

## Status (September 2026)

| Milestone | State | Still open |
|---|---|---|
| M0 Core engine + CLI | ✅ implemented | the 1-hour real-hardware exit test |
| M1 Desktop MVP | ✅ implemented | exit test with three desktops; pairing-time measurement |
| M2 Android sender | ✅ implemented | the compatibility list ([ANDROID_APPS.md](ANDROID_APPS.md)) has no tested entries yet |
| M3 iOS sender | ✅ implemented | iOS discovery (mDNS needs the multicast entitlement or a native `NWBrowser` port); UI copy about the red recording indicator; the 1-hour / memory exit test |
| M4 Mobile hub | ✅ implemented | the 2-hour screen-off exit test; battery measurements |
| M5 Polish and release | 🚧 in progress | see below |

"Implemented" means the code, unit tests and CI builds exist; the exit criteria below still have to be
checked on real devices. Details per milestone follow each section's list.

## M0: Core engine + CLI (Linux, Windows)

- Rust workspace: `hfa-proto`, `hfa-core`, `hfa-cli`.
- `hfa send --tone 440 --to <ip>` and `hfa hub`: one stream over UDP with Opus, a jitter buffer, and drift
  resampling, played through `cpal`.
- System-audio capture: PipeWire monitor on Linux, WASAPI loopback on Windows.
- Unit tests + a localhost integration test with simulated loss and jitter.

**Exit criteria:** play music on a Linux or Windows laptop and hear it on another PC's headphone for 1 hour
without glitches or drift build-up; latency without the Bluetooth hop is under 100 ms on home Wi-Fi.

*Status:* implemented (`hfa-proto`, `hfa-audio`, `hfa-capture`, `hfa-core`, `hfa-cli`; PipeWire and WASAPI
capture; `hfa selftest` with loss and jitter runs in CI). Exit test on real hardware: open.

## M1: Multi-source desktop MVP (Windows, macOS, Linux)

- The hub mixes N streams: per-source volume and mute, soft limiter, idle detection.
- macOS capture through Core Audio process taps, with the original output muted on the sender.
- mDNS discovery, then QR/PIN pairing, then Noise-encrypted control and media.
- Flutter desktop app over `flutter_rust_bridge`: hub and sender screens, tray/menu-bar mode.
- CI: `cargo test`, `clippy`, `flutter analyze`, and builds for all 3 desktop OSes.

**Exit criteria:** three desktops stream to one hub at once. Each can be adjusted on its own. The pairing
flow takes under 30 s.

*Status:* implemented (mixer with gain/mute/priority ducking/limiter, macOS process taps, mDNS, PIN/QR/link
pairing, Noise, Flutter desktop app with tray, CI on all three desktop OSes). Exit test: open.

## M2: Android sender

- Kotlin `CaptureService`: foreground service of type `mediaProjection` + `AudioPlaybackCapture` → JNI → Rust sender.
- Flutter Android UI; `MulticastLock` for discovery; clear messaging when an app blocks capture.
- A published compatibility list of popular apps (captured / blocked).

**Exit criteria:** YouTube or a game on the phone and a video call on the laptop play together in a headphone on the PC hub.

*Status:* implemented (capture service, `MulticastLock`, opt-out messaging). The compatibility list is
started in [ANDROID_APPS.md](ANDROID_APPS.md) (rules + report template); app entries are untested.

## M3: iOS sender

- ReplayKit Broadcast Upload Extension (Swift) linking the Rust **sender-only** static lib (memory < 50 MB).
- App Group for shared pairing keys; `RPSystemBroadcastPickerView` start button in the Flutter UI.
- UI copy explaining the DRM silence limit and the red recording indicator.

**Exit criteria:** audio from the iPhone (for example YouTube in Safari, games, podcasts) is heard on the hub for 1 hour; the extension is
not killed for using too much memory.

*Status:* implemented (broadcast extension with the Rust sender, App Group, picker button, DRM note).
Open: the red-indicator copy, iOS discovery (iOS pairs by QR code, link or address), the exit test.

## M4: Mobile hub ("any device is a hub")

- Android hub: receive + mix + `AudioTrack`/AAudio output as `USAGE_MEDIA` without exclusive audio focus, in a
  foreground service.
- iOS hub: background audio mode, `AVAudioSession(.playback, .mixWithOthers)`; the app keeps running while in the background.
- Battery work: DTX, batching, a lower mixer tick on idle streams.

**Exit criteria:** headphone on the phone; the laptop's meeting audio plays over the phone's own music
for 2 hours with the screen off.

*Status:* implemented (Android `HubService`, iOS background audio with `.mixWithOthers`; DTX keep-alives).
Open: the exit test and battery measurements.

## M5: Polish and release

- Automatic ducking by priority source; per-app capture selection (Windows process loopback, macOS taps).
- Adaptive bitrate and FEC; latency auto-tuning; per-source stats in the UI.
- Security hardening and review; crash reporting (opt-in).
- Distribution:
  - Windows: MSIX / installer.
  - macOS: notarized DMG.
  - Linux: Flatpak / AppImage.
  - Android: Play Store.
  - iOS: App Store (broadcast-extension review).

*Status:* in progress. Done: priority ducking; per-app capture on Windows and macOS; adaptive bitrate on
reported loss, FEC and an adaptive jitter buffer; per-source stats in the UI; installers (Inno Setup,
AppImage, Flatpak, DMG) and a tag-triggered release workflow that publishes them with the APK/AAB and
the CLI, signing where keys are configured (docs/BUILDING.md, "Releasing"). Open: store releases (Play,
App Store, Microsoft Store/MSIX, Flathub), the external security review, opt-in crash reporting.

## Future (after the MVP)

- **Microphone return path:** use the headphone mic for a call running on a *sender* device (hub mic →
  sender virtual mic). Hard on mobile, but useful for "join a laptop meeting from the phone hub".
- **Internet mode:** a relay server or WebRTC for devices that are not on the same LAN.
- **Box mode:** a Raspberry Pi image. Phones connect to it over Bluetooth as to a speaker (no app, and it
  works with DRM audio on iPhone); it mixes with PipeWire and sends to the headphone through a second adapter. Reuses `hfa-core`.
- **LE Audio / Auracast** input and output once OS APIs and headphones mature.
- Interop: accept RTP/ROC streams from PipeWire's built-in modules.

## Risks

| Risk | Impact | Mitigation |
|---|---|---|
| iOS has no real system-audio capture; DRM audio is silent | iPhone as a sender is limited | ReplayKit extension; be honest in the UI; Box mode for full iPhone support |
| Android apps opt out of playback capture | Some apps are silent on the sender | Compatibility list; recommend alternatives (the app's web version, etc.) |
| The sender device keeps playing out loud | Annoying echo in the room | macOS tap mute; investigate Windows per-session mute; user guidance on Android (M0/M2 spike) |
| Bluetooth latency (100–250 ms) + network | Video on a sender device is out of sync | Show the latency; recommend low-latency BT codecs or game mode; allow delay offsets |
| Mobile OS kills background work | Hub or sender stops | Foreground services (Android); background audio (iOS); reconnect logic |
| Battery drain on mobile | Bad reviews | DTX, 20 ms frames on battery, idle stream suspend |
| Wi-Fi congestion or multicast blocked (guest networks, AP isolation) | Discovery or streaming fails | Manual IP / QR fallback; FEC; adaptive jitter buffer |
| App Store rejection of the broadcast extension | No iOS sender | A clear use-case description; fall back to a hub-only iOS app |

## Open questions (resolve with M0/M2 spikes)

1. How do we keep the sender device's local speakers quiet on Windows and Android without silencing the capture?
2. What is the minimum stable jitter-buffer target on typical home Wi-Fi (2.4 vs 5 GHz)?
3. Should Roc Toolkit be embedded as the transport (via FFI), or is the custom UDP framing enough?
4. Licensing: MIT/Apache-2.0 for our code. Check the libopus (BSD) and Roc (MPL-2.0) obligations if we embed them.
