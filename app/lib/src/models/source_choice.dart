import 'package:flutter/foundation.dart';

import '../api/hfa_api.dart';

/// What a sender can capture, as offered in the source picker.
enum SourceKind {
  /// Android: playback capture by the native `CaptureService` (MediaProjection).
  deviceAudio,

  /// iOS: a ReplayKit screen broadcast (the extension streams).
  broadcast,

  /// Everything this device plays (Rust capture).
  system,

  /// Everything except this app (Rust capture).
  systemExceptThisApp,

  /// One app (Rust per-process capture).
  app,

  /// A test tone.
  tone,
}

/// Feed id the Android capture service pushes into (CONTRACTS.md §8.3).
const androidFeedId = 1;

/// Feed id of the short-lived sender the iOS app uses to pair before a
/// broadcast (nothing is pushed into it).
const iosPairingFeedId = 2;

/// Sample rate of the Android capture feed.
const nativeSampleRate = 48000;

/// Channels of the Android capture feed.
const nativeChannels = 2;

/// Frequency of the test tone.
const toneFrequencyHz = 440.0;

/// The sources this device can offer, best first, from the core's
/// capabilities: native capture on Android/iOS (`external_only`), the system
/// mix and single apps where Rust can capture them, and always a test tone.
List<SourceKind> availableSources(AppInfo info) {
  final caps = info.capabilities;
  return [
    if (caps.externalOnly && info.platform == 'android') SourceKind.deviceAudio,
    if (caps.externalOnly && info.platform == 'ios') SourceKind.broadcast,
    if (caps.systemMix) SourceKind.system,
    if (caps.systemMix) SourceKind.systemExceptThisApp,
    if (caps.perApp) SourceKind.app,
    SourceKind.tone,
  ];
}

/// The selected source (kind plus the app for [SourceKind.app]).
@immutable
class SourceChoice {
  /// Creates a choice.
  const SourceChoice(this.kind, {this.app});

  /// What to capture.
  final SourceKind kind;

  /// The app, for [SourceKind.app].
  final CaptureAppDto? app;

  /// Whether the choice can be started ([SourceKind.app] needs an app).
  bool get isComplete => kind != SourceKind.app || app != null;

  /// The core's capture source. The Android and iOS kinds use an external
  /// feed that native code fills.
  CaptureSourceDto toDto() {
    return switch (kind) {
      SourceKind.system => const CaptureSourceDto.system(),
      SourceKind.systemExceptThisApp =>
        const CaptureSourceDto.systemExcludingSelf(),
      SourceKind.app => CaptureSourceDto.process(pid: app?.pid ?? 0),
      SourceKind.tone => const CaptureSourceDto.tone(freqHz: toneFrequencyHz),
      SourceKind.deviceAudio => const CaptureSourceDto.external_(
        feedId: androidFeedId,
        sampleRate: nativeSampleRate,
        channels: nativeChannels,
      ),
      SourceKind.broadcast => const CaptureSourceDto.external_(
        feedId: iosPairingFeedId,
        sampleRate: nativeSampleRate,
        channels: nativeChannels,
      ),
    };
  }

  /// Label shown on the hub (empty = the core's default for the source).
  String get label => switch (kind) {
    SourceKind.app => app?.name ?? '',
    SourceKind.deviceAudio || SourceKind.broadcast => 'Device audio',
    _ => '',
  };

  @override
  bool operator ==(Object other) =>
      other is SourceChoice && other.kind == kind && other.app == app;

  @override
  int get hashCode => Object.hash(kind, app);
}

/// Title of a [SourceKind] in the picker.
String sourceTitle(SourceKind kind) => switch (kind) {
  SourceKind.deviceAudio => "This device's audio",
  SourceKind.broadcast => 'Screen broadcast (all app audio)',
  SourceKind.system => 'Everything this device plays',
  SourceKind.systemExceptThisApp => 'Everything except this app',
  SourceKind.app => 'One app',
  SourceKind.tone => 'Test tone (440 Hz)',
};

/// One-line explanation of a [SourceKind].
String sourceSubtitle(SourceKind kind) => switch (kind) {
  SourceKind.deviceAudio =>
    'Android asks for permission to capture playback. Apps that opt out '
        'of capture (and calls) stay silent.',
  SourceKind.broadcast =>
    'Start a broadcast from the button below. DRM-protected audio (e.g. '
        'Apple Music, Netflix) arrives as silence.',
  SourceKind.system => 'The full system mix.',
  SourceKind.systemExceptThisApp =>
    'The system mix without this app — use it when this device is a hub too.',
  SourceKind.app => 'Capture a single application.',
  SourceKind.tone => 'Check the connection without playing anything.',
};
