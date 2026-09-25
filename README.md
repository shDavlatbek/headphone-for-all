# headphone-for-all

**Hear every device in one headphone at the same time.**

A Bluetooth headphone plays audio from only one device at a time. "Multipoint" headphones can stay
*connected* to two devices, but they only *switch* between them and never mix them. So you can't
hear your laptop's meeting and your phone's music, or your PC game and your tablet's video, together.

`headphone-for-all` works around this in software. The headphone stays paired to **one** device,
the **hub**. Every other device runs the app, captures its own audio and streams it over Wi-Fi to the
hub. The hub mixes all the streams and plays the mix in the headphone.

```
 ┌────────────┐  capture → Opus → UDP
 │  Phone A   │──────────────────────┐
 └────────────┘                      │        ┌──────────────────────┐
 ┌────────────┐                      ├──────▶ │         HUB          │  Bluetooth   🎧
 │  Laptop B  │──────────────────────┤  LAN   │ decode → mix → play  │ ───────────▶ headphone
 └────────────┘                      │ Wi-Fi  │ (+ its own audio)    │  (one link)
 ┌────────────┐                      │        └──────────────────────┘
 │  Tablet C  │──────────────────────┘
 └────────────┘
```

The headphone sees one normal source, so no firmware hacks and no special headphones are needed.

## Status

📐 **Planning.** Research and architecture are done. Implementation starts with milestone M0 (see the roadmap).

## Planned platform support

| Platform | Send its audio (sender) | Headphone connected here (hub) | How the audio is captured |
|---|---|---|---|
| Windows 10 2004+ / 11 | ✅ M1 | ✅ M1 | WASAPI loopback / process loopback |
| macOS 14.2+ | ✅ M1 | ✅ M1 | Core Audio process taps |
| Linux (PipeWire) | ✅ M0 | ✅ M0 | PipeWire monitor source |
| Android 10+ | ✅ M2 ⚠️ some apps block capture | ✅ M4 | AudioPlaybackCapture + MediaProjection |
| iOS 15+ | ⚠️ M3 via screen-broadcast extension, DRM audio is silent | ✅ M4 | ReplayKit Broadcast Upload Extension |

Any device can be the hub. The first hub builds are desktop; the mobile hub comes in M4.

## Tech stack (short)

- **Core engine:** Rust. Handles audio I/O (`cpal`), the Opus codec, UDP transport, the jitter buffer,
  clock-drift resampling, the mixer, mDNS discovery and Noise encryption.
- **UI:** Flutter (one codebase for Windows, macOS, Linux, Android and iOS) through `flutter_rust_bridge`.
- **Native capture:** small per-OS modules (Rust for Windows, macOS and Linux; Kotlin for Android;
  Swift + a Rust static lib for the iOS broadcast extension).

## Documents

- [docs/RESEARCH.md](docs/RESEARCH.md): why headphones can't mix, the options we evaluated, the
  audio-capture APIs on each OS, codecs and transport, latency, and prior art.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): system design, tech stack, wire protocol, mixer,
  threading, repo layout, security.
- [docs/ROADMAP.md](docs/ROADMAP.md): milestones M0–M5 with exit criteria, future work, risks and open questions.
- [docs/BUILDING.md](docs/BUILDING.md): prerequisites, building and testing the core, CLI and app on every platform, packaging, CI.
