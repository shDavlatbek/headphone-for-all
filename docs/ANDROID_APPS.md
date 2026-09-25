# Android app compatibility

An Android sender captures other apps' playback with `AudioPlaybackCapture` (Android 10+). Android lets
each app decide whether it may be captured, so some apps stay **silent** on the hub while everything
else plays. This page lists the rules and what has been tested. It is the "published compatibility
list" of the roadmap (M2) and is filled in from reports.

## The rules (from the Android platform)

An app's audio can be captured only when all of these hold:

- Its audio usage is `USAGE_MEDIA`, `USAGE_GAME` or `USAGE_UNKNOWN`. **Calls and VoIP**
  (`USAGE_VOICE_COMMUNICATION`), alarms, notifications and ringtones are never captured.
- It has not opted out: no `android:allowAudioPlaybackCapture="false"` in its manifest and no
  `setAllowedCapturePolicy(ALLOW_CAPTURE_BY_NONE)` / `ALLOW_CAPTURE_BY_SYSTEM` on the stream.
- It targets Android 10 (API 29) or newer, or it opted in explicitly. Apps that still target API 28 or
  lower are **not** captured by default.

Apps that play DRM-protected video often opt out. Nothing on the sender can override an opt-out.

## Workarounds for a silent app

1. Use the service's **website in a browser** (Chrome, Firefox) instead of its app: that often works,
   because the app's own opt-out does not apply there (DRM-protected web video may still be silent).
   Not verified for every service yet, see the table.
2. Play the content on another device that is a better sender (a computer), or make the phone the
   **hub** instead: the hub plays its own audio directly into the headphone together with the mix.
3. For calls, make the phone the hub, or take the call on the device the headphone is connected to.

## Tested apps

Status: ✅ captured · ❌ silent (blocked) · ⚠️ partly (e.g. ads or some content silent) · ❔ not tested yet.
Please report results with the "App compatibility report" issue template, including the app version
and the Android version.

| App | Category | Status | App version / Android version tested | Notes / workaround |
|---|---|---|---|---|
| YouTube | video | ❔ | — | if silent: youtube.com in Chrome |
| YouTube Music | music | ❔ | — | |
| Spotify | music | ❔ | — | if silent: open.spotify.com in Chrome |
| Netflix | video (DRM) | ❔ | — | DRM video apps usually opt out |
| Prime Video | video (DRM) | ❔ | — | |
| Twitch | streaming | ❔ | — | |
| Chrome (web audio/video) | browser | ❔ | — | the usual workaround for silent apps |
| Firefox | browser | ❔ | — | |
| Pocket Casts | podcasts | ❔ | — | |
| Games (Unity / Unreal titles) | games | ❔ | — | games normally use `USAGE_GAME` |
| WhatsApp / Telegram / Meet calls | calls | ❌ | platform rule | call audio is never capturable |

Entries move from ❔ only with a report that names the app version and the Android version.
