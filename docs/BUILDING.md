# Building, testing and packaging

How to build every part of headphone-for-all from source: the Rust core (`core/`), the `hfa`
command-line tool, the Flutter app (`app/`) on all five platforms, the generated
flutter_rust_bridge bindings and the installers. The same commands run in CI
(`.github/workflows/`, see [Continuous integration](#continuous-integration)).

Design and contracts live in [ARCHITECTURE.md](ARCHITECTURE.md) and [CONTRACTS.md](CONTRACTS.md);
this file is only about building.

- [Toolchain versions](#toolchain-versions)
- [Prerequisites per OS](#prerequisites-per-os)
- [Rust core](#rust-core)
- [The `hfa` CLI and the selftest](#the-hfa-cli-and-the-selftest)
- [Flutter app](#flutter-app)
- [flutter_rust_bridge codegen](#flutter_rust_bridge-codegen)
- [Android notes](#android-notes)
- [iOS notes and the broadcast extension](#ios-notes-and-the-broadcast-extension)
- [macOS notes](#macos-notes)
- [Packaging](#packaging)
- [Continuous integration](#continuous-integration)
- [Troubleshooting](#troubleshooting)

## Toolchain versions

| Tool | Version | Where it is pinned |
|---|---|---|
| Rust | CI lints and tests with **1.94.1** (MSRV **1.87**, needed by `rubato` 5); any newer stable builds | `RUST_TOOLCHAIN` in `.github/workflows/rust.yml` and `flutter.yml`; MSRV: `core/Cargo.toml` `rust-version` |
| Flutter / Dart | **3.47.5** stable (Dart 3.13) | `.github/workflows/flutter.yml` `FLUTTER_VERSION`; `app/pubspec.yaml` `sdk: ^3.13.4` |
| flutter_rust_bridge | **2.13.0 exactly**: Rust crate, Dart package and `flutter_rust_bridge_codegen` | `core/Cargo.toml` (`=2.13.0`), `app/pubspec.yaml`, `flutter.yml` `FRB_VERSION` |
| Android | compile SDK 36, build-tools 36, **NDK 29.0.14206865**, `minSdk` 29, JDK 17+ | `app/android/app/build.gradle.kts` |
| Android Gradle Plugin / Gradle / Kotlin | 9.1.0 / 9.3.1 / 2.4.0 | `app/android/settings.gradle.kts`, `gradle-wrapper.properties` |
| CMake | 3.16+ (builds the bundled libopus 1.6.1 on every target) | — |
| Xcode | a current release (CI: the default Xcode of `macos-latest`) | — |

**The Rust release of CI.** The Rust workflow and the codegen drift check use the pinned
`RUST_TOOLCHAIN` (not `stable`), so a new Rust release cannot turn CI red through new clippy lints
under `-D warnings` or a changed rustfmt output in the generated bindings. Run the local gates with the
same release (`rustup toolchain install 1.94.1`, then `cargo +1.94.1 clippy …`, or make it your
default). Bump it on purpose: install the new release locally, run the fmt/clippy/test gates (including
the Windows cross-check) and `flutter_rust_bridge_codegen generate`, fix what they report, and change
`RUST_TOOLCHAIN` in both workflows in the same commit. The Flutter build jobs install `stable`, because
cargokit always builds `hfa-ffi` with rustup's `stable` channel; they do not lint.

## Prerequisites per OS

Every platform needs **Rust** through [rustup](https://rustup.rs) (cargokit, the Flutter ↔ Cargo glue,
calls `rustup` to add targets), **CMake** and a C compiler: the `bundled-opus` feature compiles libopus
from source.

### Linux (Debian / Ubuntu)

```sh
# Rust core: PipeWire capture (pipewire-sys runs bindgen → libclang), cpal's ALSA backend, libopus (CMake)
sudo apt-get install libasound2-dev libpipewire-0.3-dev libspa-0.2-dev libclang-dev clang pkg-config cmake
# Flutter Linux desktop: GTK 3 + ninja; tray_manager's cnativeapi also needs X11 and Xi
sudo apt-get install ninja-build libgtk-3-dev libx11-dev libxi-dev liblzma-dev libstdc++-12-dev
# Optional: live PipeWire tests, integration tests without a display, Windows cross-checks
sudo apt-get install pipewire pipewire-bin wireplumber dbus-user-session xvfb mingw-w64
```

At run time the app and `hfa` need a **PipeWire** session (PipeWire 0.3.x or 1.x with WirePlumber;
every current desktop distribution has one) for capture, and ALSA/PipeWire for playback.

### Windows 10 2004+ / 11

- Visual Studio 2022 or newer with the **"Desktop development with C++"** workload (MSVC, Windows SDK,
  CMake). Flutter's Windows build and the Rust MSVC toolchain both use it.
- Rust `stable-x86_64-pc-windows-msvc` (the rustup default on Windows).
- Git with long paths enabled (`git config --global core.longpaths true`); keep the checkout path short,
  for example `C:\src\hfa` (see [Troubleshooting](#troubleshooting)).
- Packaging only: [Inno Setup 6.3+](https://jrsoftware.org/isinfo.php) (the build script installs it
  with Chocolatey when `ISCC.exe` is missing).

### macOS 14.2+ (Apple Silicon or Intel)

- Xcode (from the App Store) with its command-line tools, then `sudo xcodebuild -runFirstLaunch`.
- CMake (`brew install cmake`) and CocoaPods (`brew install cocoapods`).
- Rust targets used by the release builds: `rustup target add aarch64-apple-darwin x86_64-apple-darwin
  aarch64-apple-ios aarch64-apple-ios-sim` (cargokit adds missing ones itself, but a manual add avoids
  surprises offline).
- System-audio capture uses Core Audio process taps, which exist from **macOS 14.2**; older systems can
  still run a hub.

### Android (host: Linux, macOS or Windows)

- Android SDK with `platforms;android-36`, `build-tools;36.0.0`, `platform-tools` and
  **`ndk;29.0.14206865`** (`sdkmanager "ndk;29.0.14206865"`; Gradle can also download it).
- JDK 17 or newer (`JAVA_HOME`), e.g. Temurin 17 or the JBR that ships with Android Studio.
- Rust targets `aarch64-linux-android armv7-linux-androideabi x86_64-linux-android`.
- For plain cargo builds of the Android code: `cargo install cargo-ndk --locked`.

### iOS

A Mac with the macOS prerequisites above. Device builds that you install need an Apple Developer team
(see [iOS notes](#ios-notes-and-the-broadcast-extension)); CI builds with `--no-codesign`.

## Rust core

The Cargo workspace is `core/` (crates `hfa-proto`, `hfa-audio`, `hfa-capture`, `hfa-core`,
`hfa-ffi`, `hfa-cli`). Run the commands from the repository root:

```sh
cargo fmt   --manifest-path core/Cargo.toml --all -- --check
cargo clippy --manifest-path core/Cargo.toml --workspace --all-targets -- -D warnings
cargo test  --manifest-path core/Cargo.toml --workspace
cargo build --manifest-path core/Cargo.toml --release -p hfa-cli     # → core/target/release/hfa
```

All four must pass before a merge (CONTRACTS.md §2); CI runs clippy and the tests on Linux, Windows and
macOS.

**Features** (CONTRACTS.md §11):

- `bundled-opus` (default) builds libopus 1.6.1 from source with CMake and links it statically.
  `--no-default-features` links the system libopus instead (`libopus-dev`) and skips the C build.
- `hfa-ffi` also has `flutter` (default): the flutter_rust_bridge API. Without it only the C ABI (iOS
  broadcast extension) and the JNI exports (Android) are compiled.

**Cross-checks from Linux** (no Windows or Apple machine needed):

```sh
rustup target add x86_64-pc-windows-gnu aarch64-apple-darwin aarch64-apple-ios aarch64-linux-android
# Windows (mingw-w64 builds libopus for the GNU target)
cargo clippy --manifest-path core/Cargo.toml --workspace --all-targets --target x86_64-pc-windows-gnu -- -D warnings
# Apple: no macOS SDK on Linux, so no libopus and no flutter_rust_bridge C shims
cargo check --manifest-path core/Cargo.toml --workspace --all-targets --target aarch64-apple-darwin --no-default-features
cargo check --manifest-path core/Cargo.toml -p hfa-ffi --target aarch64-apple-ios --no-default-features
# Android (JNI code): cargo-ndk resolves the workspace from the current directory
cd core
ANDROID_NDK_HOME=$ANDROID_HOME/ndk/29.0.14206865 ANDROID_PLATFORM=android-29 \
  cargo ndk -t arm64-v8a -P 29 clippy -p hfa-ffi --all-targets -- -D warnings
```

Without cargo-ndk, point the `cc` crate and the linker at the NDK clang yourself:

```sh
TC=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin
ANDROID_PLATFORM=android-29 CC_aarch64_linux_android=$TC/aarch64-linux-android29-clang \
  AR_aarch64_linux_android=$TC/llvm-ar CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=$TC/aarch64-linux-android29-clang \
  cargo clippy --manifest-path core/Cargo.toml -p hfa-ffi --all-targets --target aarch64-linux-android -- -D warnings
```

**Ignored tests.** Some tests need hardware or a service and are `#[ignore]`d; run them explicitly:

| Test | Needs | Command |
|---|---|---|
| `hfa-capture` `live_*` (Linux) | a PipeWire + WirePlumber session with a default sink named `hfa-test-sink` | see below |
| `hfa-capture` macOS tap test | a Mac with the System Audio Recording permission for the terminal | `cargo test --manifest-path core/Cargo.toml -p hfa-capture -- --ignored` |

The PipeWire live tests in a headless machine or container (CI does exactly this):

```sh
export XDG_RUNTIME_DIR=/tmp/pw-run      # keep it short: a socket path must fit in 108 bytes
mkdir -p "$XDG_RUNTIME_DIR" && chmod 700 "$XDG_RUNTIME_DIR"
dbus-run-session -- sh -c 'pipewire & sleep 1; wireplumber & sleep 1000000' &
sleep 3
pw-cli create-node adapter '{ factory.name=support.null-audio-sink node.name=hfa-test-sink
    media.class=Audio/Sink object.linger=true audio.position=[FL FR] }'
pw-metadata 0 default.audio.sink        # should name hfa-test-sink
cargo test --manifest-path core/Cargo.toml -p hfa-capture -- --ignored --test-threads=1 live_
```

**Faster local builds.** Several checkouts (git worktrees) can share one target directory:
`export CARGO_TARGET_DIR=$HOME/.cache/hfa-target CARGO_INCREMENTAL=0`.

## The `hfa` CLI and the selftest

`hfa` (crate `hfa-cli`) runs a hub or a sender without the app: handy on a headless machine and for
testing. Build it from the repository root with `cargo build --manifest-path core/Cargo.toml --release -p hfa-cli`
(the binary is `core/target/release/hfa`, or `hfa.exe`) or run it through
`cargo run --manifest-path core/Cargo.toml --release -p hfa-cli -- <args>`.

```sh
hfa hub --pair                                   # run a hub, open a pairing window, print PIN + URI
hfa hub --out wav:/tmp/mix.wav --no-mdns         # record the mix instead of playing it
hfa discover --timeout 5                         # list hubs on the LAN (mDNS)
hfa send --to 192.168.1.20 --pin 123456          # first time: pair with the PIN shown by the hub
hfa send --hub "Living room"                     # later: find a trusted hub by name or id
hfa send --uri 'hfa://pair?...' --source system-excl   # pair from the URI / QR text
hfa send --to hub.local:47810 --source tone:440 --bitrate 96000 --frame-ms 20
hfa devices                                      # outputs, capturable apps, capture capabilities
hfa trust list                                   # paired devices
hfa trust remove ab12-cd34-ef56-7890
```

- Sources: `system` (default), `system-excl` (everything except this program), `pid:<n>` (one app),
  `tone:<hz>`, `wav:<path>`. Outputs (`hub --out`): `default`, `device:<name>`, `wav:<path>`, `null`.
- Global options: `--data-dir <DIR>` (settings, identity, trusted peers; default: the platform data
  directory) and `-v` / `-vv` (`RUST_LOG` overrides them). `hfa <command> --help` lists every option.
- Default port 47810 (TCP control + UDP media). Allow it through the firewall on the hub.

**Selftest.** `hfa selftest` starts an in-process hub (WAV/null output) and a tone sender over
localhost, optionally with simulated loss and jitter, then reports latency, loss and glitches. It exits
non-zero when the result is out of bounds; CI runs:

```sh
cargo run --release -p hfa-cli --manifest-path core/Cargo.toml -- selftest --seconds 8 --loss 5 --jitter 20
```

(`--seconds` 1..=600, default 5 (the hub's WAV takes about 23 MB per minute); `--loss` 0..=100 %; `--jitter` extra delay in ms, uniform 0..=N.)

## Flutter app

The app is `app/` (package `headphone_for_all`, app id `io.github.shdavlatbek.hfa`). It loads the Rust
core through flutter_rust_bridge; **cargokit** (`app/rust_builder/`) builds `core/hfa-ffi` as part of every
`flutter build` / `flutter run`, so there is no separate Rust step.

```sh
cd app
flutter pub get
flutter analyze                  # must report no issues
flutter test                     # widget tests (fake core, no Rust library)
flutter run -d linux             # or windows / macos / an Android or iOS device id
flutter run --dart-define=HFA_FAKE=true   # demo mode: the whole UI on a fake core, no Rust
```

**Integration test** against the real Rust library (a desktop device or a phone):

```sh
flutter test integration_test -d linux          # needs a display; headless: xvfb-run -a flutter test ...
```

**Release builds** (run each on its own OS):

| Command | Output |
|---|---|
| `flutter build linux --release` | `app/build/linux/x64/release/bundle/` (`headphone_for_all` + `lib/` + `data/`) |
| `flutter build windows --release` | `app/build/windows/x64/runner/Release/` (`headphone_for_all.exe`, `hfa_ffi.dll`, plugin DLLs, `data\`) |
| `flutter build macos --release` | `app/build/macos/Build/Products/Release/headphone_for_all.app` (universal: arm64 + x86_64) |
| `flutter build apk --release` | `app/build/app/outputs/flutter-apk/app-release.apk` (arm64-v8a, armeabi-v7a, x86_64) |
| `flutter build apk --debug --target-platform android-arm64` | a faster debug APK, arm64 only |
| `flutter build appbundle --release` | `app/build/app/outputs/bundle/release/app-release.aab` (Play Store) |
| `flutter build ios --release --no-codesign` | `app/build/ios/iphoneos/Runner.app` (unsigned; also builds the broadcast extension) |
| `flutter build ipa` | a signed `.ipa` (needs your team, see iOS notes) |

The Android release build is signed with the **debug key** until a release keystore is configured in
`app/android/app/build.gradle.kts`; such an APK installs for testing but cannot go to a store.

**Clean up.** A release build keeps a full Rust target directory under `app/build/` (cargokit), several
GB per platform. Remove build outputs with:

```sh
rm -rf app/build app/.dart_tool/flutter_build app/android/.gradle app/android/app/build
```

## flutter_rust_bridge codegen

The Dart bindings in `app/lib/src/rust/**` (including the `*.freezed.dart` files) and
`core/hfa-ffi/src/frb_generated.rs` are **generated and committed**. Never edit them by hand.
Regenerate them after any change to `core/hfa-ffi/src/api/**` (the only codegen input, see
`app/flutter_rust_bridge.yaml`):

```sh
cargo install flutter_rust_bridge_codegen --version 2.13.0 --locked   # or: cargo binstall flutter_rust_bridge_codegen@2.13.0
cargo install cargo-expand --locked    # the codegen expands hfa-ffi with it (it installs it itself when missing)
cd app
flutter pub get
flutter_rust_bridge_codegen generate   # rewrites lib/src/rust/** and core/hfa-ffi/src/frb_generated.rs
git status                             # commit everything it changed
```

- The codegen runs `cargo expand` on `hfa-ffi` with `--cfg frb_expand` (a full `cargo check` of the
  dependency tree the first time) and then `build_runner` for the freezed classes, so it needs the Rust
  build prerequisites of your OS and a resolved `flutter pub get`.
- CI runs the same command and fails when `git status` is not clean afterwards (**drift check**). The
  generated code depends on the Dart formatter, so generate with Flutter 3.47.5.
- **Upgrading flutter_rust_bridge** is one change in four places, all to the same version:
  `core/Cargo.toml` (`flutter_rust_bridge = "=X.Y.Z"`), `app/pubspec.yaml` (`flutter_rust_bridge: X.Y.Z`),
  `FRB_VERSION` in `.github/workflows/flutter.yml` and your installed codegen; then regenerate and commit.
  Dependabot ignores this package for that reason.

## Android notes

- **NDK.** The app pins `ndkVersion = "29.0.14206865"`. cargokit's Gradle plugin builds `hfa-ffi` with
  the NDK that Gradle selected and sets `ANDROID_NDK_HOME`, `ANDROID_NDK_ROOT` and
  `ANDROID_PLATFORM=android-<minSdk>` for the build (a feat/ffi patch of cargokit), because opusic-sys
  builds libopus with CMake and the NDK's toolchain file. IDE builds and `flutter run` therefore need no
  exports. Plain cargo / cargo-ndk builds do: set `ANDROID_NDK_HOME` and `ANDROID_PLATFORM=android-29`
  yourself (see [Rust core](#rust-core)).
- **ABIs.** Release APKs contain arm64-v8a, armeabi-v7a and x86_64 (each a separate Rust build). Pass
  `--target-platform android-arm64` for quicker local builds; a debug build without it also adds x86_64
  for the emulator. 32-bit x86 is not supported.
- **One native library.** `libhfa_ffi.so` holds both the flutter_rust_bridge API and the JNI exports
  (`io.github.shdavlatbek.hfa.NativeBridge.pushPcm` / `pushPcm16`) that the capture service calls.
- **Kotlin unit tests and lint** (JVM tests of the capture state machine; run after one `flutter build apk`
  or `flutter pub get` created `gradlew` and `local.properties`):

  ```sh
  cd app/android
  ./gradlew -Ptarget-platform=android-arm64 :app:testDebugUnitTest :app:lintDebug
  ```

- **Runtime.** Capture needs Android 10 (API 29) or newer and the user's MediaProjection consent; apps
  that opt out of playback capture stay silent. A hub runs as a foreground service.

## iOS notes and the broadcast extension

- The app (`Runner`) links `libhfa_ffi.a` through the cargokit pod (with the flutter_rust_bridge API).
- A phone **sends** its audio through the ReplayKit broadcast upload extension, Xcode target
  **`HfaBroadcast`** (bundle id `io.github.shdavlatbek.hfa.broadcast`, sources in `app/ios/HfaBroadcast/`).
  Its first build phase, `app/ios/scripts/build_rust_ext.sh`, compiles `hfa-ffi` a second time **with only
  the C ABI** (`cargo rustc --crate-type staticlib --no-default-features --features bundled-opus`) into
  `$BUILT_PRODUCTS_DIR/libhfa_ext.a`, in its own cargo target directory. So `flutter build ios` needs the
  Rust targets `aarch64-apple-ios` (device) and `aarch64-apple-ios-sim` / `x86_64-apple-ios` (simulator);
  the script adds missing ones with rustup and sources `~/.cargo/env` (Xcode build phases do not see
  your shell's `PATH`).
- A Rust static library does not carry its framework dependencies: both targets link `AVFAudio`,
  `AudioToolbox`, `CoreAudio`, `CoreFoundation`, `Foundation`, `-lobjc` (and the extension `-liconv`,
  `CoreMedia`, `ReplayKit`). When Apple-side Rust dependencies change, compare with
  `cargo rustc -p hfa-ffi --target aarch64-apple-ios --lib --crate-type staticlib -- --print native-static-libs`.
- App and extension share identity and pairings through the **App Group**
  `group.io.github.shdavlatbek.hfa`. To install on a device: give both App IDs
  (`io.github.shdavlatbek.hfa`, `io.github.shdavlatbek.hfa.broadcast`) the App Groups capability with
  that group, select your team for **both** targets in `app/ios/Runner.xcworkspace`, then
  `flutter run --release` or `flutter build ipa`. Without the App Group (unsigned builds) the app falls
  back to its own container and the extension cannot use the app's pairing.
- ReplayKit broadcasts do not run in the Simulator; audio from DRM-protected apps is silent. The
  extension must stay under ~50 MB of memory.
- The Xcode project is edited only by the committed Ruby scripts in `app/ios/scripts/`
  (`gem install xcodeproj`; `ruby app/ios/scripts/add_broadcast_extension.rb` is idempotent,
  `verify_xcodeproj.rb` checks the invariants). Details: `app/ios/README.md`.
- XCTest (`RunnerTests`, also covers the extension's PCM converter): after one
  `flutter build ios --simulator --debug`, run
  `xcodebuild test -workspace ios/Runner.xcworkspace -scheme Runner -destination 'platform=iOS Simulator,name=<iPhone>' CODE_SIGNING_ALLOWED=NO`
  from `app/`.
- mDNS discovery on iOS needs the restricted multicast entitlement; until the app has it, senders on iOS
  connect by address or QR code.

## macOS notes

- The app is sandboxed (network client/server, audio input). Capturing system audio triggers the
  **System Audio Recording** permission prompt (`NSAudioCaptureUsageDescription`) on first use; if you
  denied it, re-enable it in System Settings → Privacy & Security → Screen & System Audio Recording.
- Release builds are universal; cargokit builds `hfa-ffi` for `aarch64-apple-darwin` and
  `x86_64-apple-darwin` and `lipo`s them.
- Local builds are signed "to run locally". Distribution outside the App Store needs a Developer ID
  signature and notarization, which `packaging/macos/build-dmg.sh --sign … --notarize` does.

## Packaging

The desktop installers are built from the release builds above by the scripts in `packaging/`
(details and options: `packaging/README.md`). Every script writes into `packaging/dist/` and takes the
version from `app/pubspec.yaml`.

| Artifact | Command (after `flutter build <platform> --release`) | File |
|---|---|---|
| Windows installer (Inno Setup) | `pwsh packaging/windows/build-installer.ps1` (`-Build` also runs the Flutter build) | `Headphone_for_All-<ver>-windows-x64-setup.exe` |
| Linux AppImage | `packaging/linux/build-appimage.sh` | `Headphone_for_All-<ver>-x86_64.AppImage` |
| Linux Flatpak | `packaging/linux/build-flatpak.sh` | `Headphone_for_All-<ver>-x86_64.flatpak` |
| macOS disk image | `packaging/macos/build-dmg.sh [--sign ID] [--notarize]` | `Headphone_for_All-<ver>-macos.dmg` |
| Android | `flutter build apk --release` / `flutter build appbundle` | see [Flutter app](#flutter-app) |
| iOS | `flutter build ipa` (signed) | — |

The Linux packages do not bundle GTK 3 or **libpipewire-0.3**: they come from the host, so that the
library matches the running PipeWire daemon. glibc is not bundled either, so build the AppImage on the
oldest distribution you want to support. CI builds it in an `ubuntu:22.04` container (glibc 2.35,
libpipewire 0.3.48) on an `ubuntu-latest` runner, so the AppImage runs on Ubuntu 22.04, Debian 12 and
newer.

## Continuous integration

GitHub Actions runs `rust.yml` and `flutter.yml` on pushes to `main` and `claude/**`, on every pull
request (so a `feat/**` branch gets CI through its pull request) and on demand (`workflow_dispatch`).
Changes to Markdown files and `docs/` alone do not trigger it. A newer push to the same branch cancels
the older run (except on `main`). Workflows have read-only repository access (only the publish job of
`release.yml` may write, to create the release).

**`rust.yml`**

| Job | Runner | What |
|---|---|---|
| rustfmt | ubuntu-latest | `cargo fmt --check` (every Rust job uses `RUST_TOOLCHAIN`, see [Toolchain versions](#toolchain-versions)) |
| clippy + test | ubuntu, windows, macos (-latest) | `clippy --workspace --all-targets -D warnings`, `cargo test --workspace` (the first real run of the WASAPI and Core Audio backends' unit tests) |
| Android | ubuntu-latest | `cargo ndk -t arm64-v8a clippy -p hfa-ffi` with the runner's newest NDK |
| iOS | macos-latest | `cargo check` + `clippy -p hfa-ffi --target aarch64-apple-ios` (default features: bundled libopus + frb) |
| CLI selftest | ubuntu-latest | `hfa selftest --seconds 8 --loss 5 --jitter 20` (release) |
| PipeWire live tests | ubuntu-latest | headless PipeWire + WirePlumber with a null sink, then the `live_*` tests |

**`flutter.yml`**

| Job | Runner | What | Artifact |
|---|---|---|---|
| analyze + test | ubuntu-latest | `flutter analyze`, `flutter test` | — |
| flutter_rust_bridge drift check | ubuntu-latest | `flutter_rust_bridge_codegen generate`, then `git status` must be clean | — |
| Linux build | ubuntu-latest | integration test under `xvfb-run`, `flutter build linux --release` | `headphone_for_all-linux-x64` (tar.gz) |
| Linux AppImage | ubuntu-latest, `ubuntu:22.04` container | its own `flutter build linux --release` on the oldest supported glibc, then `packaging/linux/build-appimage.sh`; does nothing until that script is on the branch | `headphone_for_all-linux-appimage` |
| Linux Flatpak | ubuntu-latest, Flathub's `flatpak-github-actions:freedesktop-26.08` container (privileged) | `packaging/linux/build-flatpak.sh` on the Linux build's bundle (after its integration test) | `headphone_for_all-linux-flatpak` |
| Android APK | ubuntu-latest | JDK 17, SDK 36 + NDK 29.0.14206865, `flutter build apk --release`, Kotlin unit tests + lint if present; release runs also `flutter build appbundle --release` | `headphone_for_all-android-apk`, release runs `…-android-aab` |
| Windows build | windows-latest | `flutter build windows --release`, Inno Setup installer if `packaging/windows/build-installer.ps1` exists | `headphone_for_all-windows-x64` (zip), `…-windows-x64-setup` |
| macOS build | macos-latest | `flutter build macos --release`, DMG if `packaging/macos/build-dmg.sh` exists | `headphone_for_all-macos` (zipped .app), `…-macos-dmg` |
| iOS build | macos-latest | `flutter build ios --release --no-codesign` (Runner + broadcast extension) | — |
| XCTest | macos-latest | `RunnerTests` on an iOS simulator and on macOS; the simulator is an iPhone of the newest iOS runtime the selected Xcode's SDK supports (`.github/scripts/pick_ios_simulator.py`, unit-tested in the same job) | — |

Artifacts are kept for 14 days (Actions → the run → Artifacts). The builds of ordinary runs are unsigned
(the APK is debug-signed): for testing, not for distribution. Tagged releases are published as GitHub
releases, see [Releasing](#releasing).

**Dependabot** (`.github/dependabot.yml`) opens weekly update PRs for Cargo (`core/`), pub (`app/`) and
the workflow actions, grouping minor/patch updates; flutter_rust_bridge is excluded (see
[codegen](#flutter_rust_bridge-codegen)).

**Checking workflow changes locally:**

```sh
# https://github.com/rhysd/actionlint/releases (uses shellcheck when it is on PATH)
actionlint
python3 -c "import yaml,glob; [yaml.safe_load(open(f)) for f in glob.glob('.github/**/*.yml', recursive=True)]"
```

## Releasing

`.github/workflows/release.yml` runs on a pushed tag `v<version>` (and on demand, without publishing):

1. **Version check:** the tag must be `v` + the `version:` of `app/pubspec.yaml` (without `+build`), and
   that must equal `version` in `core/Cargo.toml` (`[workspace.package]`). Bump both (and the pubspec
   build number) in one commit before tagging.
2. **App packages:** calls `flutter.yml` with `release: true`, i.e. every job of an ordinary run (so the
   analysis, tests and codegen drift check gate the release) plus the Android App Bundle and signing
   where the secrets below are set.
3. **CLI:** `hfa` for `linux-x86_64` (built in an `ubuntu:22.04` container; needs glibc ≥ 2.35 and the
   host's libpipewire-0.3), `windows-x86_64` (MSVC) and `macos-universal` (lipo of both Mac
   architectures), each archived with the licence files.
4. **Publish** (tags only): a GitHub release with generated notes (a tag with a `-`, e.g. `v1.2.0-beta.1`,
   becomes a pre-release), the files below and `SHA256SUMS`.

| File | Signed when |
|---|---|
| `Headphone_for_All-<ver>-windows-x64-setup.exe`, `…-windows-x64-portable.zip` | `WINDOWS_CERTIFICATE_*` |
| `Headphone_for_All-<ver>-x86_64.AppImage`, `…-x86_64.flatpak`, `…-linux-x64.tar.gz` | — (unsigned) |
| `Headphone_for_All-<ver>-macos.dmg` | `MACOS_*` (+ notarized with `APPLE_API_*`) |
| `Headphone_for_All-<ver>-android.apk`, `…-android.aab` | `ANDROID_*` (else debug-signed, with a warning) |
| `hfa-<ver>-<platform>.tar.gz` / `.zip` | — (unsigned) |

Repository secrets (Settings → Secrets and variables → Actions); each group is optional, and only
release runs read them:

| Secret | Use |
|---|---|
| `ANDROID_KEYSTORE_BASE64`, `ANDROID_KEYSTORE_PASSWORD`, `ANDROID_KEY_ALIAS`, `ANDROID_KEY_PASSWORD` | the upload/release keystore (`base64 -w0 release.jks`); passed to Gradle as `HFA_ANDROID_KEYSTORE*` / `HFA_ANDROID_KEY_*` |
| `WINDOWS_CERTIFICATE_PFX_BASE64`, `WINDOWS_CERTIFICATE_PASSWORD` | Authenticode: `signtool` signs `headphone_for_all.exe` and `hfa_ffi.dll` before packaging, then the setup `.exe` (timestamp: DigiCert) |
| `MACOS_CERTIFICATE_P12_BASE64`, `MACOS_CERTIFICATE_PASSWORD`, `MACOS_SIGN_IDENTITY` | Developer ID Application certificate, imported into a temporary keychain; `build-dmg.sh` signs the app (hardened runtime) and the DMG |
| `APPLE_API_KEY_P8_BASE64`, `APPLE_API_KEY_ID`, `APPLE_API_ISSUER` | App Store Connect API key for `notarytool`; with a signing identity, the DMG is notarized and stapled |

**Android signing locally:** `app/android/app/build.gradle.kts` signs release builds with the key from
`app/android/key.properties` (`storeFile` relative to `app/android/app`, `storePassword`, `keyAlias`,
`keyPassword`; git-ignored) or from the environment (`HFA_ANDROID_KEYSTORE` = path,
`HFA_ANDROID_KEYSTORE_PASSWORD`, `HFA_ANDROID_KEY_ALIAS`, `HFA_ANDROID_KEY_PASSWORD`, which defaults to the
store password). Without a key it falls back to the debug key and warns;
`HFA_ANDROID_REQUIRE_RELEASE_KEY=true` makes that an error. A keystore without password or alias is
always an error. Check with `./gradlew :app:signingReport` (from `app/android`).

Not automated yet: store uploads (Google Play, App Store/TestFlight: iOS is only built unsigned),
Flathub submission, and signing of the AppImage and of the CLI binaries.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `Unable to find libclang` / `pipewire-sys` build script fails | Install `libclang-dev clang` (Linux); set `LIBCLANG_PATH` to the directory with `libclang.so` if it is in an unusual place. |
| `The system library libpipewire-0.3 (or libspa-0.2) required by crate … was not found` | Install `libpipewire-0.3-dev libspa-0.2-dev pkg-config`. |
| `Neither the NDK or a standalone toolchain was found` (opusic-sys / CMake, Android) | Set `ANDROID_NDK_HOME` and `ANDROID_PLATFORM=android-29` for cargo / cargo-ndk builds. Flutter builds set them through cargokit; if one fails anyway, check that `ndk;29.0.14206865` is installed. |
| `could not find Cargo.toml` from `cargo ndk --manifest-path …` | cargo-ndk reads the workspace from the current directory: `cd core` first. |
| `Content hash on Dart side is different from Rust side` at start-up, or Dart compile errors in `lib/src/rust/` | The bindings are stale or were generated with another frb version: run `flutter_rust_bridge_codegen generate` (2.13.0) and rebuild. |
| The codegen drift check reports stale bindings although nothing changed, or frb warns `Fail to format` | flutter_rust_bridge_codegen formats `frb_generated.rs` with `rustfmt` and only warns when that fails; install it (`rustup component add rustfmt`) and generate with the `RUST_TOOLCHAIN` release, since another rustfmt can format differently. |
| CI clippy fails on code that passes locally | CI uses `RUST_TOOLCHAIN` (see [Toolchain versions](#toolchain-versions)); lint with the same release: `cargo +1.94.1 clippy …`. |
| `cargo expand returned empty output` from the codegen | `hfa-ffi` does not compile (see the command output), or cargo-expand is missing / too new for the toolchain: `cargo install cargo-expand --locked`. |
| Linux build: `Package 'x11'` / `'xi'` / `'gtk+-3.0'` not found | Install `libgtk-3-dev libx11-dev libxi-dev` (tray_manager's `cnativeapi` compiles native code on every platform). |
| macOS / iOS link errors such as `Undefined symbols … _AudioObjectGetPropertyData` or `_objc_msgSend` | A framework is missing from the target that links the Rust library; compare with `--print native-static-libs` (see iOS notes). |
| Windows: `path too long`, MSBuild / CMake errors deep inside `build\windows\…\cargokit_build` | Clone to a short path (`C:\src\hfa`) and enable long paths (`git config --global core.longpaths true`, and the `LongPathsEnabled` registry setting). |
| Windows: capturing one app (or "everything except this app") fails with "process loopback needs Windows 10 build 20348 or newer" | Process loopback is officially available from build 20348 (it usually works from 19041, version 2004); update Windows or capture the whole system mix. |
| Linux: `failed to connect` / `Host is down` from `pw-cli`, or PipeWire exits with `File name too long` | No PipeWire session for this user, or `XDG_RUNTIME_DIR` is so long that the socket path exceeds 108 bytes; use a short directory such as `/tmp/pw-run`. |
| `ALSA lib … Unknown PCM` noise in test output in containers | Harmless: cpal probes ALSA devices that a container does not have. |
| The disk fills up | Each release build keeps a Rust target directory under `app/build/`; see the clean-up command in [Flutter app](#flutter-app). A shared `CARGO_TARGET_DIR` avoids one copy per checkout. |
| `Woah! You appear to be trying to run flutter as root.` | A warning only (containers); it does not affect the build. |
