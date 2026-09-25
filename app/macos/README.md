# macOS runner

On macOS the Rust core does the audio work itself: `hfa-capture` captures system audio with
Core Audio process taps (macOS 14.2+) and the hub plays through Core Audio. The native side is
small: a platform channel, Info.plist privacy strings and entitlements.

## Platform channel `hfa/platform` (macOS)

Registered in `Runner/MainFlutterWindow.swift`, implemented in `Runner/HfaPlatformChannel.swift`.

| Method | Behaviour |
|---|---|
| `getDataDir` | `~/Library/Application Support/io.github.shdavlatbek.hfa/hfa` (created; inside the sandbox container `~/Library/Containers/io.github.shdavlatbek.hfa/Data/...`). Same place as the Dart fallback (`path_provider` + `/hfa`). |
| `captureSupport` | `{supported: true, reason: "processTap"}` on macOS 14.2+, else `{supported: false, reason: "<needs macOS 14.2...>"}` |
| `startSystemCapture` | `false` (not used: Rust captures directly) |
| other §8.3 methods | no-op (`nil`) |

The event channel is not registered (Dart listens to it only on Android and iOS).

## Info.plist

- `NSAudioCaptureUsageDescription`: shown by the "System Audio Recording" prompt the first time a
  tap is created ("sends what this Mac plays to your headphone hub").
- `NSMicrophoneUsageDescription`: defensive. Taps are read through a private aggregate *input*
  device; if a macOS version routes that through the microphone permission, a missing purpose
  string would crash the app. The text says the microphone is never recorded.
- `NSLocalNetworkUsageDescription`, `NSBonjourServices = [_hfa._tcp]`: macOS 15+ local network
  privacy (mDNS discovery / advertising and the UDP media stream).
- `CFBundleName` / `CFBundleDisplayName` = "Headphone for All".

## Entitlements and the App Sandbox

`DebugProfile.entitlements` and `Release.entitlements` keep the **App Sandbox on** and add
`com.apple.security.network.client`, `com.apple.security.network.server` (control channel, UDP
media, mDNS) and `com.apple.security.device.audio-input` (DebugProfile also keeps Flutter's
`cs.allow-jit`).

Research (2026-09): do Core Audio process taps work in a sandboxed app?
- insidegui/AudioCap — the reference our Rust backend translates — ships **sandboxed**
  (checked on its `main` branch): its `AudioCap.entitlements` has `com.apple.security.app-sandbox = true`
  and `com.apple.security.device.audio-input = true` with the hardened runtime, and it creates a
  process tap plus a private aggregate device exactly like `hfa-capture/src/macos.rs`. So taps
  are not blocked by the sandbox; what they need is the TCC "System Audio Recording" consent
  (`NSAudioCaptureUsageDescription`) and the audio-input entitlement.
- Reports exist of CATap behaviour being "fragile" under the sandbox (for example DGR Labs'
  2026 write-up disables it for a v1 without a Mac App Store target), without a concrete
  failure mode.
- Our other native needs are sandbox-compatible with the network entitlements: sockets, mDNS
  multicast (`mdns-sd`), `getifaddrs`, Core Audio output. Process names for per-app capture come
  from the HAL (`kAudioProcessPropertyBundleID`); `proc_name` may be refused for other
  processes in the sandbox, which only affects the display-name fallback.

Decision: keep the sandbox (it keeps a Mac App Store build possible and limits damage) with the
entitlements above. **If manual testing on a Mac shows taps failing only because of the sandbox**
(silence or `AudioHardwareCreateProcessTap`/aggregate-device errors that disappear unsandboxed),
set `com.apple.security.app-sandbox` to `false` in both entitlements files (this is a
Developer ID / notarized DMG build, not a Mac App Store one) and document the macOS version here.

## Deployment target

`MACOSX_DEPLOYMENT_TARGET` stays at Flutter's default (12.0 in this project, pods 10.15): the app
runs as a hub on older Macs. Taps need macOS 14.2 at run time; the Rust backend resolves the tap
functions with `dlsym` and returns `CaptureError::Unsupported` ("needs macOS 14.2 or later") on
older systems, and `captureSupport` reports the same to the UI.

## Xcode project

`scripts/configure_xcodeproj.rb` (xcodeproj gem) adds `HfaPlatformChannel.swift` to the Runner
target, then lists targets / build phases and checks the entitlements settings; `--check` only
verifies. Idempotent; the resulting `project.pbxproj` is committed.

```sh
gem install xcodeproj
ruby app/macos/scripts/configure_xcodeproj.rb --check
```

## What is verified where

- Linux: plists/entitlements parse, the Ruby script passes, `cargo check --target
  aarch64-apple-darwin --no-default-features` of the Rust side.
- macOS CI (`macos-latest`): `flutter build macos` compiles the Swift code, builds hfa-ffi through
  cargokit (bundled libopus) and links the app.
- A Mac only (manual): permissions and real capture, below.

## Manual test (a Mac with macOS 14.2+, and a hub)

1. `cd app && flutter run -d macos` (or open `build/macos/Build/Products/Release/headphone_for_all.app`).
2. Allow "local network" if macOS 15 asks. Pair with a hub on the LAN (or make this Mac the hub
   and pair a phone to it).
3. Start sending "System audio": macOS asks for **System Audio Recording** (the text of
   `NSAudioCaptureUsageDescription`). Allow it; play something: it is heard on the hub's
   headphones and muted locally (tap mute behaviour).
4. Per-app capture: pick one app from the list; only that app is sent.
5. Deny/revoke in System Settings → Privacy & Security → Screen & System Audio Recording →
   "System Audio Recording Only": the app shows the permission error (or silence on some
   versions) and does not crash.
6. On macOS < 14.2 (or a VM): `captureSupport` reports unsupported, hub mode still works.
7. Sandbox check: `codesign -d --entitlements - build/macos/Build/Products/Release/headphone_for_all.app`
   lists the sandbox, network and audio-input entitlements; repeat step 3 — if capture only works
   with the sandbox off, apply the fallback described above.
