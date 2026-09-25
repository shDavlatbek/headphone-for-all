/// Dart side of the native platform channel (docs/CONTRACTS.md §8.3).
///
/// Android (`MainActivity.kt`) and iOS/macOS (`AppDelegate.swift`) implement
/// `MethodChannel('hfa/platform')` and `EventChannel('hfa/platform/events')`.
/// Windows and Linux do not register them: every call then hits a
/// [MissingPluginException], which this wrapper turns into a no-op result
/// ("not needed on this platform").
library;

import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

/// Kind of a [NativeEvent].
enum NativeEventType {
  /// Android: the capture service stopped (user revoked it, or it ended).
  captureStopped,

  /// Android: capture failed.
  captureError,

  /// iOS: the broadcast extension started.
  broadcastStarted,

  /// iOS: the broadcast extension finished.
  broadcastFinished,

  /// An event type this app version does not know.
  unknown,
}

/// An event sent by native code on `hfa/platform/events`.
@immutable
class NativeEvent {
  /// Creates an event.
  const NativeEvent(this.type, {this.message});

  /// Parses the `{type, message?}` map sent by native code.
  factory NativeEvent.fromMap(Object? raw) {
    if (raw is! Map) return const NativeEvent(NativeEventType.unknown);
    final type = switch (raw['type']) {
      'captureStopped' => NativeEventType.captureStopped,
      'captureError' => NativeEventType.captureError,
      'broadcastStarted' => NativeEventType.broadcastStarted,
      'broadcastFinished' => NativeEventType.broadcastFinished,
      _ => NativeEventType.unknown,
    };
    final message = raw['message'];
    return NativeEvent(type, message: message is String ? message : null);
  }

  /// What happened.
  final NativeEventType type;

  /// Optional human-readable detail.
  final String? message;

  @override
  bool operator ==(Object other) =>
      other is NativeEvent && other.type == type && other.message == message;

  @override
  int get hashCode => Object.hash(type, message);

  @override
  String toString() => 'NativeEvent($type, $message)';
}

/// Result of [NativeChannel.captureSupport].
@immutable
class CaptureSupport {
  /// Creates a result.
  const CaptureSupport({required this.supported, required this.reason});

  /// Native capture works on this device.
  final bool supported;

  /// Why (or how) — e.g. `broadcast` on iOS, an API-level note on Android.
  final String reason;
}

/// The hub target handed to the iOS broadcast extension.
@immutable
class BroadcastConfig {
  /// Creates a config.
  const BroadcastConfig({
    required this.hubHost,
    required this.hubPort,
    this.hubDeviceId,
    this.hubKey,
    required this.label,
  });

  /// Hub host; empty = find it by [hubDeviceId].
  final String hubHost;

  /// Hub port; 0 = default.
  final int hubPort;

  /// Hub device id (must be trusted).
  final String? hubDeviceId;

  /// Hub static key, base64url.
  final String? hubKey;

  /// Label shown on the hub.
  final String label;

  /// The channel arguments.
  Map<String, Object?> toMap() => {
    'hubHost': hubHost,
    'hubPort': hubPort,
    'hubDeviceId': hubDeviceId,
    'hubKey': hubKey,
    'label': label,
  };
}

/// Typed wrapper around `MethodChannel('hfa/platform')` and its event channel.
class NativeChannel {
  /// Creates the wrapper; the channels can be replaced in tests.
  ///
  /// [eventsSupported] says whether the native side registers the event
  /// channel (Android and iOS do); it defaults to the current target platform.
  NativeChannel({
    MethodChannel? methods,
    EventChannel? events,
    bool? eventsSupported,
  }) : _methods = methods ?? const MethodChannel(methodChannelName),
       _events = events ?? const EventChannel(eventChannelName),
       _eventsSupported =
           eventsSupported ??
           (!kIsWeb &&
               (defaultTargetPlatform == TargetPlatform.android ||
                   defaultTargetPlatform == TargetPlatform.iOS));

  /// Name of the method channel.
  static const methodChannelName = 'hfa/platform';

  /// Name of the event channel.
  static const eventChannelName = 'hfa/platform/events';

  final MethodChannel _methods;
  final EventChannel _events;
  final bool _eventsSupported;
  Stream<NativeEvent>? _eventStream;
  bool? _available;

  /// Invokes [method]; returns `null` when the platform has no handler.
  Future<T?> _invoke<T>(String method, [Object? arguments]) async {
    try {
      final result = await _methods.invokeMethod<T>(method, arguments);
      _available = true;
      return result;
    } on MissingPluginException {
      _available = false;
      return null;
    }
  }

  /// Whether the native side implements the channel (probed once with
  /// `getDataDir`, or known from an earlier call).
  Future<bool> isAvailable() async {
    final known = _available;
    if (known != null) return known;
    await getDataDir();
    return _available ?? false;
  }

  /// Native data directory (Android `filesDir/hfa`, the iOS App Group
  /// container, macOS Application Support), or `null` where the platform does
  /// not provide one (then use path_provider).
  Future<String?> getDataDir() => _invoke<String>('getDataDir');

  /// Android: asks for MediaProjection consent and starts the capture service
  /// pushing PCM into feed [feedId]. Returns `true` when capture started;
  /// `false` when refused or unavailable (always `false` on iOS and desktop).
  Future<bool> startSystemCapture({
    required int feedId,
    required int sampleRate,
    required int channels,
  }) async {
    final started = await _invoke<bool>('startSystemCapture', {
      'feedId': feedId,
      'sampleRate': sampleRate,
      'channels': channels,
    });
    return started ?? false;
  }

  /// Android: stops the capture service.
  Future<void> stopSystemCapture() => _invoke<void>('stopSystemCapture');

  /// Android: foreground hub service + multicast lock; iOS: audio session.
  Future<void> startHubService() => _invoke<void>('startHubService');

  /// Stops what [startHubService] started.
  Future<void> stopHubService() => _invoke<void>('stopHubService');

  /// Android: holds a `WifiManager.MulticastLock` (mDNS discovery).
  Future<void> acquireMulticastLock() => _invoke<void>('acquireMulticastLock');

  /// Releases the multicast lock.
  Future<void> releaseMulticastLock() => _invoke<void>('releaseMulticastLock');

  /// iOS: writes `broadcast_config.json` for the broadcast extension.
  Future<void> writeBroadcastConfig(BroadcastConfig config) =>
      _invoke<void>('writeBroadcastConfig', config.toMap());

  /// Whether native capture is supported, or `null` where the platform has no
  /// native capture (desktop: Rust captures directly).
  Future<CaptureSupport?> captureSupport() async {
    final raw = await _invoke<Map<Object?, Object?>>('captureSupport');
    if (raw == null) return null;
    final reason = raw['reason'];
    return CaptureSupport(
      supported: raw['supported'] == true,
      reason: reason is String ? reason : '',
    );
  }

  /// Native events (a broadcast stream). Empty where the native side has no
  /// event channel: listening to an unregistered `EventChannel` would report
  /// a `MissingPluginException` as a Flutter error, so it is only listened to
  /// when [isAvailable] and the platform sends events.
  Stream<NativeEvent> get events {
    return _eventStream ??= _nativeEvents().asBroadcastStream();
  }

  Stream<NativeEvent> _nativeEvents() async* {
    if (!_eventsSupported || !await isAvailable()) return;
    yield* _events.receiveBroadcastStream().map(NativeEvent.fromMap);
  }
}
