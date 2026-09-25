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

🧪 **Pre-release.** The Rust core, the headless `hfa` CLI and the Flutter app are implemented for all
five platforms (roadmap milestones M0–M4; M5 polish and distribution is in progress), with installers
and CI builds. Nothing has been released yet, and the builds have not been through the long-run
exit tests of the roadmap on real hardware. Expect rough edges.

What has been exercised where:

- **Rust core:** unit and loopback tests run in CI on Linux, Windows and macOS, plus a lossy end-to-end
  `hfa selftest` and live PipeWire capture tests on Linux.
- **Desktop app:** built in CI for Linux (with an integration test), Windows and macOS.
- **Android:** the APK is built and the Kotlin unit tests run in CI; capture and hub playback need a real device
  for a full check.
- **macOS, iOS:** built in CI, with the XCTest unit tests on the iOS simulator and macOS. Core Audio process taps
  (macOS) and the ReplayKit broadcast extension (iOS) still need a test on real devices: see the manual test steps
  in [`app/macos/README.md`](app/macos/README.md) and [`app/ios/README.md`](app/ios/README.md).

## Platform support

| Platform | Send its audio (sender) | Headphone connected here (hub) | How the audio is captured |
|---|---|---|---|
| Windows 10 2004+ / 11 | ✅ | ✅ | WASAPI loopback / process loopback |
| macOS 12+ | ✅ macOS 14.2+ | ✅ | Core Audio process taps |
| Linux (PipeWire) | ✅ | ✅ | PipeWire monitor of the default sink |
| Android 10+ | ✅ ⚠️ apps that opt out of capture stay silent; on Android 15 QPR1+ locking the screen stops it¹ | ✅ | AudioPlaybackCapture + MediaProjection |
| iOS 15+ | ⚠️ through the screen-broadcast extension; DRM audio is silent | ✅ | ReplayKit Broadcast Upload Extension |

¹ Android 15 QPR1 and newer end every screen/audio capture (MediaProjection) when the screen locks, including the
screen-off timeout. Keep the phone unlocked while it streams; after a lock, unlock it and tap Start again.

Any device can be the hub.

**Known limitations**
- **iOS has no network discovery yet:** an iPhone or iPad does not list hubs and is not found as a hub;
  connect with the hub's QR code, pairing link or address.
- **The sender keeps playing out loud** on Windows, Linux and Android (only macOS mutes the original
  output while it captures). Turning the sender's own volume down or muting it is safe: the capture
  does not depend on it.
- DRM-protected audio (e.g. Apple Music, Netflix) is silent in an iOS broadcast; some Android apps
  (and calls) block capture, see [docs/ANDROID_APPS.md](docs/ANDROID_APPS.md).
- The hub listens on TCP + UDP port 47810 (IPv4 and IPv6), but discovery (mDNS) announces its IPv4
  addresses only. Guest networks and access points with client isolation block it: use "Add by address"
  or a network without isolation.
- The CLI and the app keep separate identities and pairings (different data directories).
- No store releases, no crash reporting yet.

## Install

- **Released builds:** tagged versions are published on the
  [GitHub releases page](https://github.com/shDavlatbek/headphone-for-all/releases) (Windows installer,
  AppImage, Flatpak, macOS DMG, Android APK, and the `hfa` CLI), with `SHA256SUMS`.
- **Test builds:** every CI run on `main` keeps its packages for 14 days (Actions → a "Flutter" run →
  Artifacts; needs a GitHub login). They are unsigned; the APK is debug-signed.
- **From source:** [docs/BUILDING.md](docs/BUILDING.md) (core, CLI and app on every platform) and
  [packaging/README.md](packaging/README.md) (installers).

How to pair devices, grant the capture permissions and fix network problems:
**[docs/USER_GUIDE.md](docs/USER_GUIDE.md)**.

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
- [docs/USER_GUIDE.md](docs/USER_GUIDE.md): installing and using the app, permissions per platform, network troubleshooting.
- [docs/ANDROID_APPS.md](docs/ANDROID_APPS.md): which Android apps can be captured.
