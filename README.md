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

🧪 **Implemented, not yet released.** The Rust engines (capture, Opus, encrypted UDP transport, jitter buffer,
drift correction, mixer, pairing, mDNS), the `hfa` command-line tool and the Flutter app for all five platforms
are in this repository, and CI builds every platform. What has been exercised where:

- **Rust core:** unit and loopback tests run in CI on Linux, Windows and macOS, plus a lossy end-to-end
  `hfa selftest` and live PipeWire capture tests on Linux.
- **Desktop app:** built in CI for Linux (with an integration test), Windows and macOS.
- **Android:** the APK is built and the Kotlin unit tests run in CI; capture and hub playback need a real device
  for a full check.
- **macOS, iOS:** built in CI, with the XCTest unit tests on the iOS simulator and macOS. Core Audio process taps
  (macOS) and the ReplayKit broadcast extension (iOS) still need a test on real devices: see the manual test steps
  in [`app/macos/README.md`](app/macos/README.md) and [`app/ios/README.md`](app/ios/README.md).

Known limitations: iOS cannot find hubs on the network by itself yet (no native Bonjour; add a hub by address or
QR code), and DRM-protected audio is silent in an iOS broadcast. Building from source:
[`docs/BUILDING.md`](docs/BUILDING.md); milestones: [`docs/ROADMAP.md`](docs/ROADMAP.md).

## Platform support

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

## Try it

The headless `hfa` command runs a hub or a sender without the app (Linux, Windows, macOS).
Build it from the repository root (Linux needs `cmake`, `clang`, `pkg-config`,
`libpipewire-0.3-dev` and `libasound2-dev`):

```sh
cargo build --release --manifest-path core/Cargo.toml -p hfa-cli   # → core/target/release/hfa
```

**1. On the machine your headphone is connected to, start a hub** and open a pairing window:

```sh
hfa hub --pair                  # plays the mix on the default output
hfa hub --pair --out device:"USB Audio"   # or a named output (see `hfa devices`)
```

It prints a 6-digit PIN, a `hfa://pair?...` URI and its QR code (for the app), then a live table of
the connected sources (gain, mute, priority, loss, jitter, buffer, latency, level). Each pairing
window works for one device; with `--pair` a new one opens after every pairing and when a window
expires unused. After a wrong PIN the window is not renewed (restart `hfa hub --pair` to pair more
devices), so nobody can keep guessing. Ctrl+C stops the hub (press it again to force-quit).

**2. On every other machine, send its audio** (the PIN is needed only the first time):

```sh
hfa send --to 192.168.1.20 --pin 123456          # by address (port 47810 by default)
hfa send --hub "Desk PC" --pin 123456            # or by hub name / device id (mDNS)
hfa send --uri 'hfa://pair?v=0&h=…'              # or with the URI printed by the hub
hfa send --hub "Desk PC" --source system-excl    # later: no PIN; everything but this app
hfa send --to desk.local --source pid:4242 --bitrate 96000   # one app only
```

`--source` is `system` (default), `system-excl`, `pid:<n>`, `tone:<hz>` or `wav:<file>`. The sender
prints its state and a status line every 2 s and exits with an explanation if pairing is needed,
fails, or the hub's key does not match.

**Other commands:** `hfa discover` lists hubs on the network, `hfa devices` shows output devices,
capturable apps and what this OS can capture, `hfa trust list` / `hfa trust remove <id>` manage
paired devices (stop a running `hfa hub` / `hfa send` of the same data directory before removing a
device). `--data-dir <dir>` selects another identity/settings directory, `-v`/`-vv`/`-vvv` or
`RUST_LOG` turn on logging (`NO_COLOR` turns off its colours).

**Selftest** (no audio hardware or network needed): an in-process hub plus tone senders that pair
with a PIN and stream through a simulated lossy, jittery network; the hub's output is analysed for
missing tones, glitches and end-to-end latency, and the exit code tells whether it passed.

```sh
hfa selftest                                   # 2 senders, 5 s, clean network
hfa selftest --seconds 10 --loss 5 --jitter 30 # 5 % loss, 0-30 ms random delay (reordering)
hfa selftest --senders 4 --wav mix.wav         # 4 senders, keep the hub's output
```

## Documents

- [docs/RESEARCH.md](docs/RESEARCH.md): why headphones can't mix, the options we evaluated, the
  audio-capture APIs on each OS, codecs and transport, latency, and prior art.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): system design, tech stack, wire protocol, mixer,
  threading, repo layout, security.
- [docs/ROADMAP.md](docs/ROADMAP.md): milestones M0–M5 with exit criteria, future work, risks and open questions.
- [docs/BUILDING.md](docs/BUILDING.md): prerequisites, building and testing the core, CLI and app on every platform, packaging, CI.
