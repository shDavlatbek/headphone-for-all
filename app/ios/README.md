# iOS runner and broadcast extension

The iOS side of headphone-for-all (docs/CONTRACTS.md §8.1 C ABI, §8.3 platform channel). An iPhone
can be a **hub** (the Flutter app plays the mix in the background) or a **sender** (a ReplayKit
broadcast upload extension streams what the phone plays).

## Layout

| Path | What |
|---|---|
| `Runner/AppDelegate.swift` | registers the platform channel on the implicit Flutter engine |
| `Runner/HfaPlatformChannel.swift` | `MethodChannel('hfa/platform')`, `EventChannel('hfa/platform/events')`, Darwin-notification observer |
| `Runner/BroadcastPickerFactory.swift` | platform view `hfa/broadcast_picker` (`RPSystemBroadcastPickerView`) |
| `Runner/Info.plist`, `Runner/Runner.entitlements` | background audio, local network + Bonjour, camera (QR), App Group |
| `Shared/HfaShared.swift` | compiled into both targets: App Group id, file names, notification names, JSON formats |
| `HfaBroadcast/SampleHandler.swift` | the extension (`RPBroadcastSampleHandler`) driving the Rust sender |
| `HfaBroadcast/PcmInterleaver.swift` | ReplayKit PCM (Int16/Int32/Float32/Float64, either endianness, interleaved or not) → interleaved Float32 |
| `HfaBroadcast/Info.plist`, `.entitlements`, `-Bridging-Header.h`, `.xcconfig` | extension metadata; the bridging header imports `core/hfa-ffi/include/hfa_ext.h` |
| `scripts/build_rust_ext.sh` | first build phase of the extension: builds `hfa-ffi` into `$BUILT_PRODUCTS_DIR/libhfa_ext.a` |
| `scripts/add_broadcast_extension.rb` | adds everything above to `Runner.xcodeproj` (idempotent; the result is committed) |
| `scripts/verify_xcodeproj.rb` | prints targets / build phases and checks the project invariants |
| `RunnerTests/RunnerTests.swift` | XCTest: config/status JSON formats and the PCM converter |
| `Runner/PrivacyInfo.xcprivacy`, `HfaBroadcast/PrivacyInfo.xcprivacy` | privacy manifests (Resources of both targets): no tracking, no collected data; required-reason APIs FileTimestamp `C617.1` (Rust `std::fs` metadata → `stat`/`fstat`) and SystemBootTime `35F9.1` (cpal's Core Audio backend → `mach_absolute_time`). Re-check with `nm -u` of the built binaries when dependencies change |
| `Identity.xcconfig` | `HFA_BUNDLE_ID` (app), `HFA_BROADCAST_BUNDLE_ID` (extension), `HFA_APP_GROUP`; override them in a git-ignored `Identity.local.xcconfig` |

## Platform channel (iOS)

| Method | Behaviour |
|---|---|
| `getDataDir` | `<App Group>/hfa` (created). Without the App Group (a build not signed with the App Groups capability) `FlutterError("NO_APP_GROUP")`, which the app shows as a start-up error: a private directory would hide the pairings from the extension. Only Simulator builds fall back to Application Support `/hfa` |
| `startHubService` / `stopHubService` | `AVAudioSession` category `.playback` with `.mixWithOthers`, `setActive(true)` / `setActive(false, .notifyOthersOnDeactivation)`; errors → `FlutterError("AUDIO_SESSION")` |
| `writeBroadcastConfig` | `{hubHost, hubPort, hubDeviceId?, hubKey?, label}` → `<container>/broadcast_config.json` with the C ABI keys plus `data_dir`. `hubHost` is required (`BAD_ARGS`: the extension cannot look for hubs, see Known limitations); `FlutterError("NO_APP_GROUP")` without the App Group |
| `captureSupport` | `{supported: true, reason: "broadcast"}` |
| `startSystemCapture` | `false` (the user starts the broadcast with the picker) |
| `stopSystemCapture`, `acquireMulticastLock`, `releaseMulticastLock` | no-op |
| `getBroadcastStatus` | `{broadcasting, state?, message?, timestamp?}`: whether a broadcast runs now, plus the extension's last `broadcast_status.json` |

**Broadcast state re-sync.** Besides forwarding the Darwin notifications (below), the channel compares
`broadcast_status.json` with the screen capture state (`UIWindowScene.screen.isCaptured`, and
`sceneCaptureState` on iOS 17+) whenever Dart starts listening, the app becomes active or the capture state
changes. A status other than `finished` while the screen is captured is a running broadcast: a new listener
gets `broadcastStarted` (the app was relaunched while the extension kept broadcasting). A status other than
`finished` while nothing is captured for 8 s means ReplayKit ended the extension without `broadcastFinished`
(memory limit, crash): the app writes a `finished` status and sends `broadcastFinished` with "The broadcast
stopped unexpectedly ...". Only changes relative to what Dart was last told are sent.

Events: `{type: "broadcastStarted"}` and `{type: "broadcastFinished", message?}`, driven by the Darwin
notifications `io.github.shdavlatbek.hfa.broadcast.started` / `.finished` that the extension posts.
Darwin notifications carry no payload, so the extension first writes
`<container>/broadcast_status.json` (`{state, message?, timestamp}`); `message` is the reason a
broadcast could not start: no App Group, no or invalid `broadcast_config.json`, hub not paired (its key
is not in the trust store), no `hub_host`, or an invalid hub key; or why it ended later: the hub refused
this device (`hfa_ext_sender_state` reported `failed`, e.g. pairing required). An unreachable hub is
**not** such a reason: the Rust sender keeps reconnecting in the background.

## Broadcast extension `HfaBroadcast`

- Bundle id `$(HFA_BROADCAST_BUNDLE_ID)` (`io.github.shdavlatbek.hfa.broadcast` by default), iOS 15, principal class `SampleHandler`,
  `RPBroadcastProcessMode = RPBroadcastProcessModeSampleBuffer`, App Group entitlement.
- `broadcastStarted` reads `broadcast_config.json` (the C ABI keys), replaces its `data_dir` with
  `<container>/hfa` resolved in the extension's own process (a stored absolute container path can be
  stale after a restore or a device migration) and calls `hfa_ext_sender_start(json)`. On failure it
  calls `finishBroadcastWithError` with an `NSError` whose description tells the user what to do
  (open the app, pair this iPhone and choose the hub).
- `processSampleBuffer(.audioApp)` converts each buffer (format read from the buffer's
  `AudioStreamBasicDescription`, samples from `CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer`)
  into reused storage and calls `hfa_ext_push_pcm` with the buffer's rate and channel count (Rust
  resamples to 48 kHz stereo). `.video` and `.audioMic` are ignored. A lock serializes pushes and
  the stop, so the handle is never used concurrently or after it was freed.
- While the sender runs, a 1 s timer polls `hfa_ext_sender_state` (under the same lock) and logs
  state changes. On `failed` (final: pairing required, key mismatch...) it stops the sender, writes
  `broadcast_status.json` with the reason, posts the finished notification and calls
  `finishBroadcastWithError`.
- `broadcastFinished` calls `hfa_ext_sender_stop`.
- Memory: ReplayKit kills upload extensions above ~50 MB. The extension has no Flutter, no video
  processing, one reused sample buffer and the Rust sender (a 2-thread tokio runtime + Opus).

### How the Rust library gets in

`scripts/build_rust_ext.sh` runs as the extension's first build phase. From `PLATFORM_NAME` /
`ARCHS` / `CONFIGURATION` it builds `hfa-ffi` with `cargo rustc --crate-type staticlib
--no-default-features --features bundled-opus` for `aarch64-apple-ios` (device) or
`aarch64-apple-ios-sim` / `x86_64-apple-ios` (simulator, `lipo`'d when both), in its own target
directory (`$PROJECT_TEMP_DIR/hfa_ext_cargo`), and copies the archive to
`$BUILT_PRODUCTS_DIR/libhfa_ext.a`. Debug → cargo dev profile, Release/Profile → `--release`
(override: `HFA_EXT_RUST_PROFILE`). It sources `~/.cargo/env`, appends `~/.cargo/bin`, `/opt/homebrew/bin` and
`/usr/local/bin` to `PATH` (a build started from the Xcode GUI has a minimal `PATH`), fails early with a clear
error when `cargo` or `cmake` (bundled libopus) is missing, builds with `--locked` (like CI: an Xcode build never
rewrites `core/Cargo.lock`) and installs missing rustup targets.

Xcode GUI builds: the app's own Rust pod (cargokit's `build_pod.sh`, in `app/rust_builder/`) also builds the
bundled libopus with CMake but does not extend `PATH`. If a GUI build fails there with "cmake not found", build
once from a terminal (`flutter build ios`; later GUI builds reuse the compiled libopus) or make Homebrew's
directory visible to GUI apps (`sudo launchctl config user path "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin"`,
then restart the Mac).

The extension links `-lhfa_ext` (`LIBRARY_SEARCH_PATHS = $(BUILT_PRODUCTS_DIR)`) plus what the
static library needs (`rustc --print native-static-libs` for this configuration): `AVFAudio`,
`AudioToolbox`, `CoreAudio`, `CoreFoundation`, `Foundation`, `-lobjc`, `-liconv`, and `CoreMedia`,
`ReplayKit` for the Swift code. Security / SystemConfiguration / libc++ / libresolv are not needed
(libopus is C, and no Rust dependency links them); re-check when Apple-side dependencies change.

**No duplicate symbols:** the Flutter app links its own `libhfa_ffi.a` (cargokit pod, with the
flutter_rust_bridge API). The extension is a separate Mach-O binary that links only
`libhfa_ext.a` (C ABI only, a different file name, a different cargo target directory), and the
Runner target never links `-lhfa_ext` (checked by `verify_xcodeproj.rb`).

### Xcode project

The project is changed only by `scripts/add_broadcast_extension.rb` (xcodeproj gem):

```sh
gem install xcodeproj
ruby app/ios/scripts/add_broadcast_extension.rb   # idempotent: re-run after `flutter create .` etc.
ruby app/ios/scripts/verify_xcodeproj.rb          # lists targets/phases, exits 1 on a problem
```

It adds the `HfaBroadcast` target (Debug/Release/Profile, base configuration
`HfaBroadcast/HfaBroadcast.xcconfig`, which includes `Flutter/Generated.xcconfig` for
`FLUTTER_BUILD_NAME`/`NUMBER` but not the CocoaPods configuration of Runner), embeds
`HfaBroadcast.appex` through an "Embed Foundation Extensions" copy-files phase (`dstSubfolderSpec
13`, placed before Flutter's "Thin Binary" phase to avoid Xcode's build-cycle error), makes Runner
depend on it, compiles the new Runner sources, sets `CODE_SIGN_ENTITLEMENTS` for Runner and
compiles `PcmInterleaver.swift` into RunnerTests as well.

### Identity (bundle ids, App Group, team)

`ios/Identity.xcconfig` is the only place that names them; Runner's `Flutter/Debug.xcconfig` /
`Release.xcconfig` and `HfaBroadcast/HfaBroadcast.xcconfig` include it:

| Setting | Default | Used by |
|---|---|---|
| `HFA_BUNDLE_ID` | `io.github.shdavlatbek.hfa` | Runner `PRODUCT_BUNDLE_IDENTIFIER` |
| `HFA_BROADCAST_BUNDLE_ID` | `$(HFA_BUNDLE_ID).broadcast` | HfaBroadcast `PRODUCT_BUNDLE_IDENTIFIER`; the `HfaBroadcastExtension` Info.plist key (the picker's `preferredExtension`, the Darwin notification names `<id>.started` / `.finished`) |
| `HFA_APP_GROUP` | `group.$(HFA_BUNDLE_ID)` | both `.entitlements` files; the `HfaAppGroup` Info.plist key (`HfaShared.appGroupId`) |

To build with another team, create `ios/Identity.local.xcconfig` (git-ignored, included at the end of
`Identity.xcconfig`):

```
HFA_BUNDLE_ID = com.example.hfa
DEVELOPMENT_TEAM = ABCDE12345
```

Xcode expands the settings in the Info.plists and the entitlements; `verify_xcodeproj.rb` checks that nothing
names the ids directly.

## What is verified where

On Linux (this repository's dev container):
- every plist / entitlements file parses (`plistlib`), the bridging header compiles as C
  (`clang -fsyntax-only -x objective-c -I core/hfa-ffi/include ...`),
- the Ruby scripts run, are idempotent and `verify_xcodeproj.rb` passes,
- `build_rust_ext.sh` passes `shellcheck` and a dry run with stub `cargo`/`rustup`/`lipo`
  (target selection, profile, lipo),
- the Rust side: `cargo check -p hfa-ffi --target aarch64-apple-ios --no-default-features`
  and the C ABI unit tests (header ↔ Rust constants).

Only on macOS (CI `macos-latest`, or a Mac):
- Swift compilation of Runner and HfaBroadcast (no `swiftc` on Linux) and the real
  `build_rust_ext.sh` run with bundled libopus: `flutter build ios --no-codesign`
  (device) and `flutter build ios --simulator` (simulator slices),
- linking the extension against `libhfa_ext.a` (missing frameworks would show up here),
- the unit tests: `xcodebuild test -workspace ios/Runner.xcworkspace -scheme Runner
  -destination 'platform=iOS Simulator,name=<an installed iPhone>'` (after one `flutter build ios --simulator`).

Only on a real iPhone (ReplayKit broadcasts do not run in the Simulator): everything below.

## Manual test (iPhone + a hub on the same Wi-Fi)

1. Signing: App IDs and App Groups belong to one team, so unless you are the owner of
   `io.github.shdavlatbek.hfa`, create `ios/Identity.local.xcconfig` with `HFA_BUNDLE_ID = <your prefix>`
   (and `DEVELOPMENT_TEAM = <your team id>`); see Identity (below). Give the App IDs `$(HFA_BUNDLE_ID)` **and**
   `$(HFA_BUNDLE_ID).broadcast` the App Groups capability with `group.$(HFA_BUNDLE_ID)` (Xcode's automatic
   signing does this). Without the App Group the app stops at start-up with `NO_APP_GROUP`.
2. `flutter run --release` on the iPhone. Allow local network access when asked.
3. Start a hub on another device (the app, or the `hfa` CLI), pair the iPhone (QR or PIN) and choose
   that hub as the target: the app writes `broadcast_config.json`.
4. Tap the broadcast button, pick "Headphone for All" (preselected), "Start Broadcast". The red
   status indicator appears, the app receives `broadcastStarted` and the hub shows a new source.
5. Play music from an app without DRM (YouTube in Safari, a podcast app): it plays in the hub's
   headphones. DRM audio (Apple Music, Netflix) is silent by design of ReplayKit.
6. Stop the broadcast (red indicator → Stop): the app receives `broadcastFinished`, the source
   disappears from the hub.
7. Start failure: on a fresh install (or after deleting the app's data) start the broadcast before
   pairing any hub, so there is no `broadcast_config.json`. iOS shows the extension's error text
   ("Open Headphone for All, pair this iPhone with your headphone hub ...") and the app receives
   `broadcastFinished` with that message.
8. Hub gone (known limitation, not a failure path): stop the hub, or make it forget the iPhone, then
   start the broadcast. It **starts anyway**: the app receives `broadcastStarted`, the hub shows
   nothing, and no error reaches the app while the extension keeps reconnecting in the background
   (the C ABI has no status query). If you only stopped the hub, start it again: the source appears
   within a few seconds.
9. Memory: in Xcode attach to `HfaBroadcast` (Debug → Attach to Process) and watch the memory
   gauge while streaming for several minutes; it must stay well below 50 MB.
10. Hub mode: start the hub in the app, lock the phone: playback continues (background audio,
   `.mixWithOthers` lets other apps keep playing).

Logs: `log stream --predicate 'subsystem BEGINSWITH "io.github.shdavlatbek.hfa"'` on a Mac with the
phone attached (Console.app works too).

## Known limitations

- Rust `tracing` output of the extension is not forwarded to os_log (the extension builds hfa-ffi
  without the `flutter` feature, whose logger does that). It goes to
  `<container>/hfa/broadcast.log` instead (capped at 256 KiB, one previous part kept as
  `broadcast.log.1`); the Swift side logs every failure with `os.Logger` (subsystem
  `io.github.shdavlatbek.hfa.broadcast`).
- A hub that disappears later is not reported to the app: the sender keeps reconnecting
  (`hfa_ext_sender_state` says `reconnecting`; only `failed` ends the broadcast).
- mDNS through the Rust `mdns-sd` crate (hub advertising on iOS, or `hub_host = ""` in the
  extension) needs the restricted `com.apple.developer.networking.multicast` entitlement on
  iOS 14+ (Apple must grant it). Without it, pairing by QR/URI with an explicit host works; the
  app must always pass `hubHost` to `writeBroadcastConfig` (the C ABI refuses an empty `hub_host`
  on iOS with `HFA_ERR_CONFIG`).
- App Store review of an audio-only broadcast extension is not guaranteed (docs/ROADMAP.md).
