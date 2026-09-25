import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/hfa_api.dart';
import '../models/hub_target.dart';
import '../models/source_choice.dart';
import '../platform/native_channel.dart';
import 'core_providers.dart';

/// Sender states in which a sender is live (`sender_start` would refuse).
const liveSenderStates = {'connecting', 'pairing', 'streaming', 'reconnecting'};

/// What the sender screen shows.
@immutable
class SenderState {
  /// Creates a state.
  const SenderState({
    required this.status,
    this.target,
    this.source,
    this.busy = false,
    this.error,
    this.broadcastReady = false,
    this.broadcasting = false,
  });

  /// The core's sender status.
  final SenderStatusDto status;

  /// The selected hub.
  final HubTarget? target;

  /// The selected source (defaults to the first available one).
  final SourceChoice? source;

  /// A start or stop is in progress.
  final bool busy;

  /// The last UI-level error (start refused, permission denied...).
  final String? error;

  /// iOS: the broadcast extension has its hub config; the picker is shown.
  final bool broadcastReady;

  /// iOS: the broadcast extension is streaming.
  final bool broadcasting;

  /// A sender is live in the core.
  bool get isLive => liveSenderStates.contains(status.state);

  /// The last start failed because the hub wants a PIN.
  bool get needsPin {
    final text = '${status.error ?? ''} ${error ?? ''}'.toLowerCase();
    return (status.state == 'failed' || error != null) && text.contains('pair');
  }

  /// A copy with the given fields replaced. [error] is always replaced;
  /// pass [clearTarget] to drop the target.
  SenderState copyWith({
    SenderStatusDto? status,
    HubTarget? target,
    bool clearTarget = false,
    SourceChoice? source,
    bool? busy,
    String? error,
    bool? broadcastReady,
    bool? broadcasting,
  }) {
    return SenderState(
      status: status ?? this.status,
      target: clearTarget ? null : (target ?? this.target),
      source: source ?? this.source,
      busy: busy ?? this.busy,
      error: error,
      broadcastReady: broadcastReady ?? this.broadcastReady,
      broadcasting: broadcasting ?? this.broadcasting,
    );
  }
}

/// The sender: hub/source selection, start/stop and live status.
final senderControllerProvider =
    NotifierProvider<SenderController, SenderState>(SenderController.new);

/// Drives the sender engine.
///
/// - Desktop: Rust captures directly (`sender_start` with the chosen source).
/// - Android: `sender_start(External{feed 1, 48 kHz, stereo})`, then the native
///   `startSystemCapture` (MediaProjection consent + `CaptureService`).
/// - iOS: the broadcast extension streams. The app pairs first if needed
///   (a short-lived sender on an unused external feed), then writes the
///   broadcast config and shows the system broadcast picker.
class SenderController extends Notifier<SenderState> {
  StreamSubscription<SenderStatusDto>? _statusSub;
  StreamSubscription<NativeEvent>? _nativeSub;
  bool _nativeCapture = false;

  HfaApi get _api => ref.read(hfaApiProvider);
  NativeChannel get _native => ref.read(nativeChannelProvider);

  @override
  SenderState build() {
    final api = ref.watch(hfaApiProvider);
    final native = ref.watch(nativeChannelProvider);
    _statusSub = api.senderEvents().listen(
      _onStatus,
      onError: (Object e) => _setError(describeError(e)),
    );
    _nativeSub = native.events.listen(_onNativeEvent);
    ref.onDispose(() {
      _statusSub?.cancel();
      _nativeSub?.cancel();
    });
    final sources = availableSources(ref.read(appInfoProvider));
    return SenderState(
      status: idleSenderStatus,
      source: sources.isEmpty ? null : SourceChoice(sources.first),
    );
  }

  void _onStatus(SenderStatusDto status) {
    if (!ref.mounted) return;
    state = state.copyWith(status: status, error: state.error);
    if (_nativeCapture && !liveSenderStates.contains(status.state)) {
      // The engine ended by itself (failed / stopped): stop the capture too.
      _nativeCapture = false;
      unawaited(_quietly(_native.stopSystemCapture));
    }
  }

  void _onNativeEvent(NativeEvent event) {
    if (!ref.mounted) return;
    switch (event.type) {
      case NativeEventType.captureStopped:
      case NativeEventType.captureError:
        if (!_nativeCapture) return;
        _nativeCapture = false;
        state = state.copyWith(
          error: event.type == NativeEventType.captureError
              ? 'Capture failed: ${event.message ?? 'unknown error'}'
              : 'Capture stopped${event.message == null ? '' : ': ${event.message}'}',
        );
        unawaited(_quietly(_api.senderStop));
      case NativeEventType.broadcastStarted:
        state = state.copyWith(broadcasting: true);
      case NativeEventType.broadcastFinished:
        state = state.copyWith(broadcasting: false, error: event.message);
      case NativeEventType.unknown:
        break;
    }
  }

  /// Selects the hub to stream to.
  void selectTarget(HubTarget? target) {
    state = target == null
        ? state.copyWith(clearTarget: true, broadcastReady: false)
        : state.copyWith(target: target, broadcastReady: false);
  }

  /// Selects what to capture.
  void selectSource(SourceChoice source) {
    state = state.copyWith(source: source, broadcastReady: false);
  }

  /// Forgets the current error.
  void clearError() => state = state.copyWith();

  /// Starts streaming to the selected hub. [pin] overrides the target's
  /// pairing secret (the user typed a PIN).
  Future<void> start({String? pin}) async {
    final baseTarget = state.target;
    final source = state.source;
    if (state.busy || baseTarget == null || source == null) return;
    if (!source.isComplete) {
      state = state.copyWith(error: 'Choose an app to capture.');
      return;
    }
    final target = pin == null ? baseTarget : baseTarget.withPin(pin);
    if (pin != null) state = state.copyWith(target: target);
    state = state.copyWith(busy: true);
    try {
      switch (source.kind) {
        case SourceKind.deviceAudio:
          await _startAndroid(target, source);
        case SourceKind.broadcast:
          await _prepareBroadcast(target, source);
        default:
          await _api.senderStart(
            target.toRequest(source.toDto(), label: source.label),
          );
      }
      if (!ref.mounted) return;
      state = state.copyWith(busy: false);
    } catch (e) {
      if (!ref.mounted) return;
      state = state.copyWith(busy: false, error: describeError(e));
    }
  }

  Future<void> _startAndroid(HubTarget target, SourceChoice source) async {
    await _api.senderStart(
      target.toRequest(source.toDto(), label: source.label),
    );
    final started = await _native.startSystemCapture(
      feedId: androidFeedId,
      sampleRate: nativeSampleRate,
      channels: nativeChannels,
    );
    if (!started) {
      await _quietly(_api.senderStop);
      throw const HfaApiException(
        'Audio capture was not allowed. Tap Start again and accept the '
        'screen-capture prompt.',
      );
    }
    _nativeCapture = true;
  }

  /// iOS: makes sure the hub trusts us (pairing through a short-lived
  /// sender when a secret is at hand), then hands the hub to the extension.
  Future<void> _prepareBroadcast(HubTarget target, SourceChoice source) async {
    if (target.pairingSecret != null) {
      final result = await _pairOnce(target, source);
      if (result.state != 'streaming') {
        throw HfaApiException(result.error ?? 'Pairing with the hub failed.');
      }
    }
    await _native.writeBroadcastConfig(
      BroadcastConfig(
        hubHost: target.host,
        hubPort: target.port,
        hubDeviceId: target.deviceId,
        hubKey: target.hubKey,
        label: ref.read(appInfoProvider).deviceName,
      ),
    );
    if (!ref.mounted) return;
    state = state.copyWith(target: target.withPin(null), broadcastReady: true);
  }

  /// Runs a sender on an empty external feed until it streams (paired and
  /// connected) or ends, then stops it. Returns the deciding status.
  Future<SenderStatusDto> _pairOnce(
    HubTarget target,
    SourceChoice source,
  ) async {
    final outcome = Completer<SenderStatusDto>();
    var first = true;
    final sub = _api.senderEvents().listen((status) {
      // The first event is the status before this start.
      if (first) {
        first = false;
        return;
      }
      if (outcome.isCompleted) return;
      if (status.state == 'streaming' ||
          status.state == 'failed' ||
          status.state == 'stopped') {
        outcome.complete(status);
      }
    });
    try {
      await _api.senderStart(
        target.toRequest(source.toDto(), label: source.label),
      );
      return await outcome.future.timeout(
        const Duration(seconds: 20),
        onTimeout: () => const SenderStatusDto(
          state: 'failed',
          error: 'The hub did not answer.',
          bitrate: 0,
          lossPct: 0,
          rttMs: 0,
          levelDb: -120,
        ),
      );
    } finally {
      await sub.cancel();
      await _quietly(_api.senderStop);
    }
  }

  /// Stops streaming (and the Android capture service).
  Future<void> stop() async {
    if (state.busy) return;
    state = state.copyWith(busy: true);
    try {
      if (_nativeCapture) {
        _nativeCapture = false;
        await _quietly(_native.stopSystemCapture);
      }
      await _api.senderStop();
      if (!ref.mounted) return;
      state = state.copyWith(busy: false);
    } catch (e) {
      if (!ref.mounted) return;
      state = state.copyWith(busy: false, error: describeError(e));
    }
  }

  void _setError(String message) {
    if (!ref.mounted) return;
    state = state.copyWith(error: message);
  }

  static Future<void> _quietly(Future<void> Function() action) async {
    try {
      await action();
    } catch (e) {
      debugPrint('sender: ${describeError(e)}');
    }
  }
}
