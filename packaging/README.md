# Packaging

This folder holds everything that turns the Flutter app (`app/`) into installable artifacts for the
desktop platforms, plus the app icon for every platform. The mobile store builds (APK/AAB, IPA) come
from `flutter build` and the `app/android` / `app/ios` projects; see `docs/BUILDING.md`.

| Artifact | Produced by | Built on (CI) | File name |
|---|---|---|---|
| App icon rasters | `icon/generate.py` | any (committed) | see [Icon](#icon) |
| Windows installer | `windows/build-installer.ps1` + `windows/hfa.iss` (Inno Setup) | `windows-latest` | `Headphone_for_All-<ver>-windows-x64-setup.exe` |
| Linux AppImage | `linux/build-appimage.sh` (appimagetool) | `ubuntu-22.04` | `Headphone_for_All-<ver>-x86_64.AppImage` |
| Linux Flatpak bundle | `linux/build-flatpak.sh` + `linux/io.github.shdavlatbek.hfa.yml` | `ubuntu-latest` | `Headphone_for_All-<ver>-x86_64.flatpak` |
| macOS disk image | `macos/build-dmg.sh` (create-dmg / hdiutil) | `macos-latest` | `Headphone_for_All-<ver>-macos.dmg` |

Every script writes into `packaging/dist/` (git-ignored) unless it is given `--output` / `-OutputDir`, and
reads the version from `app/pubspec.yaml` (`version: 1.2.3+4` → `1.2.3`) unless it is given one. Each
script packages an existing **release build**; run `flutter build <platform> --release` in `app/` first
(the Windows script can do it with `-Build`). Run the scripts from anywhere; they locate the repository
from their own path.

Identifiers used everywhere (keep them in sync with `docs/CONTRACTS.md` §8.2 / §8.10):

- Application id `io.github.shdavlatbek.hfa` (Linux GApplication id, `.desktop` / AppStream / Flatpak id,
  Windows AppUserModelID, Apple bundle id), display name **Headphone for All**, executable
  `headphone_for_all`.
- Windows single-instance mutex `io.github.shdavlatbek.hfa.SingleInstance` (runner + Inno Setup `AppMutex`),
  main window class `io.github.shdavlatbek.hfa.MainWindow`, registered window messages
  `io.github.shdavlatbek.hfa.Activate` / `io.github.shdavlatbek.hfa.Quit`, and the `--autostart` argument
  (runner + installer).

## Icon

`icon/hfa.svg` is the one source: a headphone receiving two "waves" on an indigo tile, drawn on a
256-unit grid with strokes ≥ 18 units so it stays readable at 16 px. `icon/generate.py` renders it
(cairosvg, or `rsvg-convert` as a fallback; Pillow builds the `.ico`):

```sh
pip install cairosvg Pillow        # or: apt-get install librsvg2-bin python3-pil
python3 packaging/icon/generate.py # --only windows,linux,other to limit the outputs
```

| Output | Used by |
|---|---|
| `app/windows/runner/resources/app_icon.ico` (16–256 px, 10 sizes) | Windows runner (`Runner.rc`), installer (`SetupIconFile`) |
| `app/linux/icons/hicolor/<N>x<N>/apps/io.github.shdavlatbek.hfa.png` (16–512) + `scalable/…svg` | Linux runner (installed into the bundle's `data/icons`), AppImage, Flatpak |
| `icon/out/android/res/` (`mipmap-*/ic_launcher.png`, adaptive `ic_launcher_foreground.png` + `mipmap-anydpi-v26/ic_launcher.xml` + `values/ic_launcher_background.xml`) | to be copied into `app/android/app/src/main/res/` by feat/android |
| `icon/out/ios/AppIcon.appiconset/` (opaque, full-bleed, Flutter template file names) | to replace `app/ios/Runner/Assets.xcassets/AppIcon.appiconset/` (feat/apple) |
| `icon/out/macos/AppIcon.appiconset/` (tile inset to the macOS grid, template file names) | to replace `app/macos/Runner/Assets.xcassets/AppIcon.appiconset/` (feat/apple) |
| `icon/out/png/hfa-512.png`, `hfa-1024.png` | store listings, README, web |

The tray icon (`app/assets/tray_icon.png`) belongs to the app package (feat/app) and uses the same
indigo and headphone shape.

## Windows

**Installer: `windows/hfa.iss` (Inno Setup 6.3+).** Per-user install by default (no UAC prompt; the
first page offers "install for all users"). It installs the whole Flutter release folder
(`build\windows\x64\runner\Release`: `headphone_for_all.exe`, `flutter_windows.dll`, `hfa_ffi.dll`, the
plugin DLLs and `data\`) plus the MSVC runtime DLLs, and creates a Start-menu shortcut (optional desktop
shortcut and "start when I sign in"), all carrying the app's AppUserModelID. The sign-in shortcut passes
`--autostart`, so the app starts hidden in the tray instead of opening its window at every sign-in. With an all-users install it
can add an inbound Windows Firewall rule for the program on **private** networks, which a hub needs
(without it Windows asks the first time the hub listens). A running app is quit before files are
replaced or removed: Setup asks first (OK/Cancel; OK when silent or with `/SUPPRESSMSGBOXES`), Uninstall
does it after the uninstall confirmation. Both post the registered `io.github.shdavlatbek.hfa.Quit` message
to the main window (also when it is hidden in the tray), which quits at once, and wait up to 10 s for the
single-instance mutex to go away. `AppMutex` stays as the fallback (for example an instance started as
administrator, which a per-user Setup may not message); its message tells the user to use **Quit** in
the tray icon's menu, since closing the window only hides it there while the hub runs. The runner also
exits on `WM_ENDSESSION`, so Restart Manager (`CloseApplications=yes`, sign-out) can close it. Uninstalling keeps the user data in
`%APPDATA%\io.github.shdavlatbek\Headphone for All\` unless the user agrees to delete it. The minimum OS
is Windows 10 2004 (process-loopback capture).

**Build script: `windows/build-installer.ps1`** (PowerShell 7 or Windows PowerShell 5.1):

```powershell
pwsh packaging/windows/build-installer.ps1 -Build            # flutter build + installer
pwsh packaging/windows/build-installer.ps1 -Version 1.2.3    # package an existing build
```

It stages the release folder, copies the `Microsoft.VC14x.CRT` DLLs of the newest Visual Studio found by
`vswhere` next to the executable (app-local deployment, so no VC++ redistributable is needed), installs
Inno Setup with Chocolatey when `ISCC.exe` is missing, and runs
`ISCC /DAppVersion=… /DAppArch=x64 /DSourceDir=… /DOutputDir=… hfa.iss`. `-Arch arm64` packages an
arm64 build. The version must be numeric (Windows version resources).

CI sketch (`windows-latest`, which has Visual Studio with the C++ workload):

```yaml
- uses: subosito/flutter-action@v2
  with: { channel: stable }
- uses: dtolnay/rust-toolchain@stable
- run: pwsh packaging/windows/build-installer.ps1 -Build
- uses: actions/upload-artifact@v4
  with: { name: windows-installer, path: packaging/dist/*.exe }
```

Code signing is not configured yet. To sign, run `signtool sign /fd sha256 /tr <timestamp url> /td sha256`
on `headphone_for_all.exe` and `hfa_ffi.dll` before `ISCC`, and on the setup `.exe` after it (Inno's
`SignTool=` directive can do the latter), with the certificate from a CI secret.

**MSIX (later).** The [`msix`](https://pub.dev/packages/msix) pub package can produce an MSIX from the
same release build (`dart run msix:create`). It needs an `msix_config` block in `app/pubspec.yaml`
(feat/app owns it), e.g. `display_name: Headphone for All`, `identity_name: io.github.shdavlatbek.hfa`,
`publisher: CN=…` (must match the signing certificate or the Store identity),
`logo_path: packaging/icon/out/png/hfa-512.png`, `capabilities: internetClientServer,privateNetworkClientServer`,
and `store: true` for a Store upload. Differences to keep in mind: an MSIX app's `%APPDATA%` is
virtualized per package (a different data folder than the Inno Setup install), Store builds must not add
firewall rules or autostart entries themselves (use the `startup_task` option of `msix_config`; a
startup task cannot pass `--autostart`, so the runner would have to check the activation kind
(`AppInstance.GetActivatedEventArgs`, `ActivationKind::StartupTask`) to start hidden), and the
single-instance mutex keeps working unchanged.

## Linux

Files shared by both formats:

- `linux/io.github.shdavlatbek.hfa.desktop`: launcher (`Icon=io.github.shdavlatbek.hfa`,
  `StartupWMClass=io.github.shdavlatbek.hfa`, which is the runner's `g_set_prgname`, so windows group
  with the launcher). Validate with `desktop-file-validate`.
- `linux/io.github.shdavlatbek.hfa.metainfo.xml`: AppStream metadata for software centers. Validate with
  `appstreamcli validate --no-net`. Before a public release add `<screenshots>` and a `<release>` entry
  per version.

**Runtime dependencies.** The Flutter bundle contains the app, the Flutter engine and the plugin
libraries (`libhfa_ffi.so` links libopus statically). From the system it needs GTK 3, GLib, libepoxy,
fontconfig and X11/Xi (every Flutter Linux app does) and **`libpipewire-0.3.so.0`**: `hfa-capture` links
libpipewire dynamically, and it has to match the running PipeWire daemon and its SPA plugins, so it is
**never bundled**; capture needs a PipeWire session (any current desktop distribution). Playback goes
through ALSA/PulseAudio (`cpal`), i.e. `pipewire-pulse` or PulseAudio.

**AppImage: `linux/build-appimage.sh`.**

```sh
(cd app && flutter build linux --release)
packaging/linux/build-appimage.sh            # → packaging/dist/Headphone_for_All-<ver>-x86_64.AppImage
```

AppDir layout: the bundle in `usr/lib/headphone-for-all/`, a `usr/bin/headphone_for_all` symlink, an
`AppRun` that execs the real binary, the `.desktop` file and 256 px icon at the root (+ `.DirIcon`), and
the `.desktop`, AppStream (`usr/share/metainfo/io.github.shdavlatbek.hfa.appdata.xml`) and hicolor icons
under `usr/share`. The script checks the bundle, warns about libraries the build machine cannot resolve,
refuses a bundled libpipewire, downloads `appimagetool` (continuous build from
github.com/AppImage/appimagetool, cached in `~/.cache/hfa-packaging`) unless `$APPIMAGETOOL` or `PATH`
provides one, and always runs it with `APPIMAGE_EXTRACT_AND_RUN=1` (no FUSE needed).
`APPIMAGE_UPDATE_INFO` embeds update information (e.g. `gh-releases-zsync|…`). glibc is not bundled, so
build on the oldest distribution to support (CI: Ubuntu 22.04). The bundle is not passed through
`linuxdeploy` on purpose: GTK and libpipewire must come from the host, and the rest of the bundle is
self-contained already.

**Flatpak: `linux/io.github.shdavlatbek.hfa.yml` + `linux/build-flatpak.sh`.**

```sh
sudo apt-get install flatpak flatpak-builder
(cd app && flutter build linux --release)
packaging/linux/build-flatpak.sh             # → packaging/dist/Headphone_for_All-<ver>-x86_64.flatpak
flatpak install --user packaging/dist/Headphone_for_All-*.flatpak
```

The manifest packages the **prebuilt** release bundle (flatpak-builder builds offline, and a Flutter +
cargo build inside the sandbox would need vendored Dart and Rust dependencies). The script stages the
bundle into `linux/_flatpak/bundle`, adds the Flathub remote (`--user`), installs
`org.freedesktop.Platform`/`Sdk` **26.08** when missing and exports a single-file bundle. The runtime
provides GTK 3, libepoxy and libpipewire-0.3. Permissions (`finish-args`):

| Permission | Why |
|---|---|
| `--share=network` | UDP media, TCP control and mDNS discovery (`mdns-sd` opens its own multicast sockets; Avahi is not used, so no `org.freedesktop.Avahi` access) |
| `--socket=wayland`, `--socket=fallback-x11`, `--share=ipc`, `--device=dri` | window and GPU rendering |
| `--socket=pulseaudio` | playing the mix (hub) |
| `--filesystem=xdg-run/pipewire-0` | capturing audio with the native PipeWire API (sender) |
| `--talk-name=org.kde.StatusNotifierWatcher`, `--own-name=org.kde.*` | the tray icon (a StatusNotifierItem that owns `org.kde.StatusNotifierItem-<pid>-<n>`; Flatpak can only grant that with a `.*` prefix) |

Known Flatpak limits: inside the sandbox the app sees only its own PID namespace, and WirePlumber gives
Flatpak clients restricted PipeWire permissions, so "system audio except this app" and per-app capture
(which create links between other clients' nodes and read `/proc` of host processes) may not work there;
plain system capture and the hub do. The AppImage has no such limits. A Flathub submission would need a
from-source build (vendored pub and cargo sources) and screenshots.

## macOS

**Disk image: `macos/build-dmg.sh`** (macOS only).

```sh
(cd app && flutter build macos --release)
packaging/macos/build-dmg.sh                                   # unsigned DMG
packaging/macos/build-dmg.sh --sign "Developer ID Application: NAME (TEAMID)" --notarize
```

It copies the `.app` from `app/build/macos/Build/Products/Release/` with `ditto`, and builds a
drag-to-Applications DMG with [`create-dmg`](https://github.com/create-dmg/create-dmg)
(`brew install create-dmg`; `--skip-jenkins` on CI, where Finder cannot be scripted) or plain
`hdiutil create -format UDZO` with an `/Applications` link.

**Signing and notarization notes** (no secrets live in this repository):

1. A **Developer ID Application** certificate is needed to distribute outside the Mac App Store. In CI,
   import the `.p12` (base64 secret + password secret) into a temporary keychain, e.g. with
   `apple-actions/import-codesign-certs`, and pass the identity with `--sign` or `MACOS_SIGN_IDENTITY`.
2. The script signs inside-out (nested frameworks/dylibs, then the app) with `--options runtime`
   (hardened runtime, required for notarization) and `--timestamp`, using
   `app/macos/Runner/Release.entitlements` (owned by feat/apple: App Sandbox, network client/server,
   audio input). Then it signs the DMG.
3. `--notarize` submits the DMG with `xcrun notarytool submit --wait` and staples the ticket
   (`xcrun stapler staple`), then checks it with `spctl`. Credentials from the environment, in order of
   preference: `APPLE_NOTARY_PROFILE` (a `notarytool store-credentials` keychain profile);
   `APPLE_API_KEY_PATH` + `APPLE_API_KEY_ID` + `APPLE_API_ISSUER` (App Store Connect API key, the best
   choice for CI: write the `.p8` from a secret to a temp file); or `APPLE_ID` + `APPLE_TEAM_ID` +
   `APPLE_APP_PASSWORD` (app-specific password).
4. If notarization fails, `xcrun notarytool log <submission-id> …` shows the reasons (usually an
   unsigned nested binary or a missing hardened runtime).

## Local checks

```sh
shellcheck packaging/linux/*.sh packaging/macos/*.sh
python3 -m py_compile packaging/icon/generate.py
python3 -m unittest discover -s packaging/icon -p 'test_*.py'   # variants + committed rasters (Pillow)
desktop-file-validate packaging/linux/io.github.shdavlatbek.hfa.desktop
appstreamcli validate --no-net packaging/linux/io.github.shdavlatbek.hfa.metainfo.xml
```

After a local build, free the disk space: `rm -rf app/build app/.dart_tool/flutter_build packaging/dist`.
