# User guide

How to install Headphone for All, pair your devices, give each OS the permissions it needs, and
fix the usual network problems. The project is pre-release: see the status and known limitations in
the [README](../README.md).

## The idea in one paragraph

Pair your headphone with **one** device as usual: that device is the **hub**. Every other device runs
the app as a **sender**: it captures what it plays and streams it over the local network to the hub,
which mixes all senders with its own sound and plays the mix in the headphone. All devices must be
on the same local network (same Wi-Fi or LAN, no guest network).

## 1. Install

| OS | Package | Notes |
|---|---|---|
| Windows 10 2004+ / 11 | `Headphone_for_All-<ver>-windows-x64-setup.exe` | Installs for the current user (no admin rights). Choose "install for all users" if you want the optional firewall rule (see [Windows](#windows)). |
| Linux (PipeWire) | `…-x86_64.AppImage` or `…-x86_64.flatpak` | The AppImage is for 2024+ distributions (glibc 2.39+: Ubuntu 24.04, Debian 13, Fedora 40 or newer) and needs GTK 3 and PipeWire from the system: `chmod +x` it and run it. On older distributions use the Flatpak: `flatpak install --user Headphone_for_All-*.flatpak`. |
| macOS 12+ (sending needs 14.2+) | `…-macos.dmg` | Drag the app to Applications. Unsigned test builds are blocked at first launch: allow them in System Settings → Privacy & Security → "Open Anyway". |
| Android 10+ | `…-android.apk` | Allow installing from your browser or file manager when Android asks. |
| iOS 15+ | — | No public build yet; build it from source (docs/BUILDING.md). |

Released versions are on the project's GitHub releases page; test builds are in the CI run artifacts
(see the README). The `hfa` command-line tool (for servers and scripting) is in the same release.

## 2. First run: choose a role

The home screen asks what this device does:

- **Headphone is connected here (Hub):** open the Hub section and switch the hub on. The device plays
  the mix on its current output (your headphone) together with its own sound.
- **Send this device's audio (Sender):** open the Sender section, pick the hub and the source (the
  whole system, the system without this app, or one app where the OS supports it), and start.

You can switch roles at any time. On desktop the window can be closed while the hub or a sender runs:
the app keeps running in the tray / menu bar; use **Quit** in the tray menu to stop it.

## 3. Pair a device

Pairing happens once per sender–hub pair; after that they reconnect by themselves.

1. On the **hub**, tap **Pair a device**. It shows a QR code, a 6-digit PIN and a pairing link. The
   pairing window lasts 5 minutes (a countdown shows it; "New PIN" opens a fresh one).
2. On the **sender**, in the Sender section, either
   - pick the hub from the list of discovered hubs and enter the PIN, or
   - **Scan QR** (mobile), or
   - **Pairing link**: paste the `hfa://pair?…` link (copy it on the hub), or
   - **Add by address**: type the hub's IP address (port 47810 by default), then the PIN.
3. Both sides show "Paired". The sender connects and the hub lists it with a volume slider, mute and a
   level meter.

After 5 wrong attempts the pairing window is closed on purpose (nobody can keep guessing): open a new
one on the hub.

## 4. Per-platform notes

### Windows

- **Firewall:** the first time the hub listens, Windows asks whether to allow the app. Answering
  **Allow** needs administrator rights; if a standard user is asked, Windows creates *block* rules and
  senders cannot connect (an administrator can change it in Windows Defender Firewall → "Allow an app
  through firewall"). The all-users install can add an allow rule for **private** networks. A newly
  joined Wi-Fi is usually **Public**: set it to Private (Settings → Network & internet → the network →
  Network profile type) so the rule applies.
- **"Start in the notification area when I sign in"** (installer option) starts the app hidden in the
  tray at sign-in. It does not switch the hub on: open the app from the tray and start the hub.
- **Sender:** the captured audio still plays on the PC's own speakers; turn them down or mute the
  output device's speakers if they echo.
- Capturing one app, or everything except this app, needs Windows 10 2004 or newer.

### macOS

- **Sending needs macOS 14.2+** and the **System Audio Recording** permission: macOS asks the first time
  a sender starts. If you denied it, allow it in System Settings → Privacy & Security → Screen &
  System Audio Recording (the "System Audio Recording Only" list), then restart the sender. For the
  `hfa` command-line tool, the permission is asked for the terminal app you run it from.
- While sending, macOS mutes the Mac's own output, so there is no echo.

### Linux

- Needs **PipeWire** (the default on current Ubuntu, Fedora, Debian 12+ with the desktop, …). Sending
  captures the monitor of the default output.
- **Tray icon:** GNOME shows it only with the "AppIndicator and KStatusNotifierItem Support" extension;
  KDE, Xfce, Cinnamon and others show it directly.
- **Firewall:** if you use `ufw` or `firewalld`, allow TCP and UDP port 47810 (and mDNS, UDP 5353) on
  the hub, e.g. `sudo ufw allow 47810` and `sudo ufw allow 5353/udp`.
- In the Flatpak, "everything except this app" and single-app capture are limited by the sandbox;
  capture the whole system instead.

### Android

- Starting a sender shows Android's **screen-capture consent**: audio capture is part of it. Android
  then shows a persistent notification and a casting indicator while it captures; **Stop** in the
  notification ends it.
- Apps can opt out of being captured, and calls are never captured: those stay silent. See
  [ANDROID_APPS.md](ANDROID_APPS.md) for known apps and workarounds.
- The phone keeps playing its own audio out loud; lower its volume or plug in wired earphones.
- As a hub, allow notifications so the hub's notification (which keeps it running) is visible.

### iOS / iPadOS

- iOS has no system-audio capture for apps; sending uses a **screen broadcast**. Start it with the
  button in the Sender section (or Control Center → long-press Screen Recording → Headphone for All →
  Start Broadcast). The status bar or Dynamic Island shows the **red recording indicator** while the
  broadcast runs: that is normal and cannot be hidden. Only audio is sent, never the screen.
- **DRM-protected audio** (Apple Music, Netflix, many streaming apps) is silent in a broadcast; this is
  an iOS rule.
- Hubs on the same network are listed automatically (Bonjour). The first time, iOS asks whether Headphone
  for All may find devices on your local network: allow it (or later in Settings → Privacy & Security →
  Local Network). **Scan QR**, the pairing link and the hub's address work too.
- The broadcast streams to the hub's address as the app knew it when the broadcast started: if the hub
  gets a new address (a router restart), stop the broadcast and start it again.

## 5. Network troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| The sender does not list the hub | mDNS (UDP 5353) blocked, different networks or VLANs, guest Wi-Fi | Use **Add by address** or the QR code/link; put both devices on the same network |
| An iPhone or iPad lists no hubs (or is not found as a hub) | Local Network access denied for Headphone for All | Settings → Privacy & Security → Local Network → turn Headphone for All on, then reopen the app |
| Pairing or connecting times out | Firewall on the hub, AP / client isolation | Allow TCP+UDP 47810 on the hub ([Windows](#windows), [Linux](#linux)); turn off client isolation or use a network without it (a phone hotspot works) |
| It worked, then stopped after a network change | The hub's address changed | Discovery finds it again; for manual addresses, re-add it or give the hub a fixed IP in the router |
| Dropouts or crackles | Weak Wi-Fi, 2.4 GHz congestion | Move closer, prefer 5 GHz, or wire the hub; the hub shows loss and jitter per source |
| Sound is late compared with the sender's video | Network + Bluetooth latency (Bluetooth alone adds 100–250 ms) | Use a low-latency headphone mode or codec if the headphone has one |
| Echo in the room | The sender still plays out loud (Windows, Linux, Android) | Turn the sender's speakers down |

The hub accepts IPv4 and IPv6 connections, but discovery announces its IPv4 addresses only (type an IPv6
address with "Add by address"). Everything stays in your local network; streams are encrypted and only
paired devices are accepted.

## 6. CLI and app on the same machine

The `hfa` tool and the app keep **separate identities and pairings**, because they use different data
directories:

| OS | App | `hfa` CLI (default) |
|---|---|---|
| Linux | `~/.local/share/io.github.shdavlatbek.hfa/hfa` | `~/.local/share/headphone-for-all` |
| Windows | `%APPDATA%\io.github.shdavlatbek\Headphone for All\hfa` | `%APPDATA%\shdavlatbek\headphone-for-all\data` |
| macOS | the app's sandbox container | `~/Library/Application Support/io.github.shdavlatbek.headphone-for-all` |

So the same computer appears as two devices and has to be paired twice. To share one identity, run
the CLI with `--data-dir <the app's directory>` (Linux and Windows), and never run the CLI and the app
at the same time on the same directory.

## 7. FAQ

- **Why not just use a multipoint headphone?** Multipoint keeps two connections but plays only one
  device at a time; it never mixes.
- **Does the hub need to be a computer?** No. Android and iOS devices can be hubs too; keep the app's
  notification (Android) or the app running in the background (iOS).
- **Can I hear my phone's call on the PC hub?** No: Android and iOS never allow call audio to be
  captured.
- **Where do I report a problem or an Android app that stays silent?** Open an issue on GitHub (there is
  an "App compatibility report" template for Android apps).
