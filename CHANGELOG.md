# Changelog

All notable changes to this project are listed here. Versions follow [Semantic Versioning](https://semver.org/);
while the version is `0.x`, every release is a pre-release and anything may still change.

## [0.1.0] - 2026-09-26

First public **pre-release**. Several devices play audio at the same time in **one** headphone. The headphone
stays paired to one device, the hub. Every other device captures its own audio and streams it to the hub over the
local network, and the hub mixes all the streams into the headphone.

> **Not yet tested on real hardware.** Every build passes CI on Linux, Windows, macOS, Android and iOS, including
> an end-to-end streaming self-test through a lossy, jittery simulated network. Capturing audio on real devices,
> Bluetooth latency and long listening sessions still need testing. Please report what works and what doesn't
> (there is an issue template for app compatibility).

### Highlights

- **Any device can be the hub:** Windows, macOS, Linux, Android or iOS.
- **Senders:**
  - Windows: the whole system or one app, via WASAPI loopback.
  - macOS 14.2+: the whole system or one app, via Core Audio taps; the Mac's own output is muted while it
    sends.
  - Linux: PipeWire.
  - Android 10+: MediaProjection.
  - iOS 15+: a ReplayKit screen-broadcast extension.
- **Mixer on the hub:** volume, mute and priority per source, a master volume that is remembered, lowering other
  sources while a priority source plays, and a soft limiter.
- **Streaming:** Opus at 48 kHz stereo. An adaptive jitter buffer absorbs Wi-Fi hiccups; clock-drift correction
  and inaudible latency cuts keep the delay low. Redundancy and FEC recover lost packets. The self-test measures
  about 100 ms of delay before the Bluetooth hop.
- **Security:** pairing with a QR code, a link or a 6-digit PIN (SPAKE2, bound to a Noise XX handshake). All
  control and audio traffic is encrypted, and each device keeps a list of trusted devices with their pairing roles.
- **Discovery:** hubs are found on the local network, on iOS through native Bonjour. A hub can also be added by
  address.
- **`hfa` command-line tool:** a headless hub or sender, discovery, device listing, trust management, and
  `hfa selftest`.

### Downloads

| Platform | File |
|---|---|
| Windows 10 2004+ / 11 | `Headphone_for_All-0.1.0-windows-x64-setup.exe` (installer) or `…-windows-x64-portable.zip` |
| macOS 12+ (sending needs 14.2+) | `Headphone_for_All-0.1.0-macos.dmg` |
| Linux, 2024+ distributions | `Headphone_for_All-0.1.0-x86_64.AppImage` or `…-linux-x64.tar.gz` |
| Linux, older distributions | `Headphone_for_All-0.1.0-x86_64.flatpak` |
| Android 10+ | `Headphone_for_All-0.1.0-android.apk` |
| Command line | `hfa-0.1.0-linux-x86_64.tar.gz`, `hfa-0.1.0-windows-x86_64.zip`, `hfa-0.1.0-macos-universal.tar.gz` |

iOS is not distributed yet: there is no App Store or TestFlight build. Checksums are in `SHA256SUMS`.

### Unsigned builds

This pre-release is **not code-signed**:

- **Windows** SmartScreen warns about an unknown publisher. Choose *More info → Run anyway*.
- **macOS** Gatekeeper blocks the app on first launch. Right-click the app and choose *Open*, or allow it under
  *System Settings → Privacy & Security*.
- The **Android** APK is signed with a debug key. Uninstall it before installing a later, properly signed version.

### Known limitations

- **The sender keeps playing out loud** on Windows, Linux and Android. Only macOS mutes its own output while
  capturing. Turn the sender's volume down; the capture does not depend on it.
- **DRM-protected audio** (Apple Music, Netflix, …) is silent in an iOS broadcast. Some Android apps and calls
  block capture (see the [Android app list](https://github.com/shDavlatbek/headphone-for-all/blob/main/docs/ANDROID_APPS.md)). On Android 15 QPR1+, locking the screen stops the capture.
- **The iOS broadcast extension** streams to the hub address the app knew when the broadcast started.
- **Discovery (mDNS)** announces IPv4 addresses only. Guest networks with client isolation block it; use
  *Add by address* instead.
- **Start at sign-in** is set by the installer on Windows and in Settings on Linux. On macOS, add the app under
  *System Settings → Login Items*.

See the [user guide](https://github.com/shDavlatbek/headphone-for-all/blob/main/docs/USER_GUIDE.md) for pairing, permissions and troubleshooting.

[0.1.0]: https://github.com/shDavlatbek/headphone-for-all/releases/tag/v0.1.0
