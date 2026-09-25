import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart' show PlatformException;
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/hfa_api.dart';
import '../models/hub_target.dart';
import '../models/source_choice.dart';
import '../platform/native_channel.dart';
import 'core_providers.dart';
import 'settings_controller.dart';

/// Sender states in which a sender is live (`sender_start` would refuse).
const liveSenderStates = {'connecting', 'pairing', 'streaming', 'reconnecting'};

/// iOS pre-pairing: how often the core's sender status is polled as a
/// fallback for a status event the subscription may have missed.
const pairingPollInterval = Duration(milliseconds: 300);

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

  /// iOS pairing run in progress (its target is updated when it ends).
  bool _pairingOnly = false;

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
    final info = ref.read(appInfoProvider);
    final sources = availableSources(info);
    if (info.isAndroid) unawaited(_syncNativeCapture());
    return SenderState(
      status: idleSenderStatus,
      source: sources.isEmpty ? null : SourceChoice(sources.first),
    );
  }

  /// Android: the capture service and the Rust sender live as long as the
  /// process, this controller only as long as the Flutter UI. A new UI (the
  /// activity was recreated) takes over a running capture, stops a capture
  /// whose sender ended, and stops a sender whose capture ended unheard.
  Future<void> _syncNativeCapture() async {
    NativeCaptureStatus? native;
    try {
      native = await _native.captureStatus();
    } catch (e) {
      debugPrint('capture status: ${describeError(e)}');
    }
    if (native == null || !ref.mounted || state.busy || _nativeCapture) {
      return;
    }
    if (!native.running && native.endedWhileAway == null) return;
    SenderStatusDto? now;
    try {
      now = await _api.senderStatus();
    } catch (e) {
      debugPrint('sender status: ${describeError(e)}');
    }
    if (now == null || !ref.mounted || state.busy || _nativeCapture) return;
    final live = liveSenderStates.contains(now.state);
    if (native.running) {
      if (live) {
        _nativeCapture = true;
      } else {
        await _quietly(_native.stopSystemCapture);
      }
    } else if (live) {
      final why = native.endedWhileAway!;
      state = state.copyWith(
        error:
            'Capture stopped while the app was closed'
            '${why.isEmpty ? '' : ': $why'}',
      );
      await _quietly(_api.senderStop);
    }
  }

  void _onStatus(SenderStatusDto status) {
    if (!ref.mounted) return;
    final target = state.target;
    // Connected with a PIN or token: the hub is paired now and the one-time
    // secret is spent, so a later start must not send it again.
    final paired =
        status.state == 'streaming' &&
            target != null &&
            target.pairingSecret != null &&
            !_pairingOnly
        ? target.asPaired()
        : null;
    state = state.copyWith(status: status, target: paired, error: state.error);
    // The core saved the hub as trusted: refresh the lists that show it.
    if (paired != null) ref.invalidate(trustedPeersProvider);
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
    // The consent dialog can stay open for a while; the sender may fail
    // meanwhile (wrong PIN, pairing required, key mismatch).
    final bool started;
    try {
      started = await _native.startSystemCapture(
        feedId: androidFeedId,
        sampleRate: nativeSampleRate,
        channels: nativeChannels,
      );
    } on PlatformException catch (e) {
      // `permissionDenied` (RECORD_AUDIO refused) or `invalidArgument`.
      await _quietly(_api.senderStop);
      throw HfaApiException(e.message ?? 'Audio capture failed (${e.code}).');
    }
    if (!started) {
      await _quietly(_api.senderStop);
      throw const HfaApiException(
        'Audio capture was not allowed. Tap Start again and accept the '
        'screen-capture prompt.',
      );
    }
    // Ask the core (not the possibly late event stream) whether the sender
    // still runs; if it ended, the capture service must not keep running.
    SenderStatusDto? now;
    try {
      now = await _api.senderStatus();
    } catch (e) {
      debugPrint('sender status: ${describeError(e)}');
    }
    final live =
        ref.mounted &&
        liveSenderStates.contains(now?.state ?? state.status.state);
    if (!live) {
      await _quietly(_native.stopSystemCapture);
      throw HfaApiException(
        now?.error ?? state.status.error ?? 'The sender stopped.',
      );
    }
    _nativeCapture = true;
  }

  /// iOS: makes sure the hub trusts us (pairing through a short-lived
  /// sender when a secret is at hand), then hands the hub to the extension.
  ///
  /// The extension never pairs and needs the hub's key or a trusted device
  /// id (CONTRACTS.md §8.5), so a hub typed in by address is identified
  /// after pairing by the device the pairing added to the trust store.
  Future<void> _prepareBroadcast(HubTarget target, SourceChoice source) async {
    var deviceId = target.deviceId;
    final identified =
        target.hubKey != null || (deviceId != null && target.trusted);
    if (target.pairingSecret == null && !identified) {
      throw const HfaApiException(
        'Pair with this hub first: enter the PIN shown on it.',
      );
    }
    if (target.pairingSecret != null) {
      final before = {for (final p in await _api.trustedPeers()) p.deviceId};
      final result = await _pairOnce(target, source);
      if (result.state != 'streaming') {
        throw HfaApiException(result.error ?? 'Pairing with the hub failed.');
      }
      if (deviceId == null && target.hubKey == null) {
        final added = [
          for (final p in await _api.trustedPeers())
            if (!before.contains(p.deviceId)) p.deviceId,
        ];
        if (added.length != 1) {
          throw const HfaApiException(
            'Paired, but the hub could not be identified. Pick it from the '
            'list of hubs and try again.',
          );
        }
        deviceId = added.single;
      }
    }
    if (target.pairingSecret != null) ref.invalidate(trustedPeersProvider);
    await _native.writeBroadcastConfig(
      BroadcastConfig(
        hubHost: target.host,
        hubPort: target.port,
        hubDeviceId: deviceId,
        hubKey: target.hubKey,
        label: ref.read(appInfoProvider).deviceName,
      ),
    );
    if (!ref.mounted) return;
    state = state.copyWith(
      target: target.asPaired(deviceId: deviceId),
      broadcastReady: true,
    );
  }

  /// Runs a sender on an empty external feed until it streams (paired and
  /// connected) or ends, then stops it. Returns the deciding status.
  ///
  /// Event order is not relied upon: the subscription's first event (the
  /// status before this start) is awaited before `senderStart`, and after
  /// the start the core's status is also polled, so a status change that
  /// raced the subscription is still seen.
  Future<SenderStatusDto> _pairOnce(
    HubTarget target,
    SourceChoice source,
  ) async {
    final outcome = Completer<SenderStatusDto>();
    final subscribed = Completer<void>();
    var started = false;
    void decide(SenderStatusDto status) {
      if (!started || outcome.isCompleted) return;
      if (status.state == 'streaming' ||
          status.state == 'failed' ||
          status.state == 'stopped') {
        outcome.complete(status);
      }
    }

    _pairingOnly = true;
    final sub = _api.senderEvents().listen((status) {
      if (!subscribed.isCompleted) {
        // The status before this start.
        subscribed.complete();
        return;
      }
      decide(status);
    });
    Timer? poll;
    try {
      await subscribed.future.timeout(
        const Duration(seconds: 2),
        onTimeout: () {},
      );
      await _api.senderStart(
        target.toRequest(source.toDto(), label: source.label),
      );
      // From here on the core's status belongs to this start.
      started = true;
      Future<void> check() async {
        try {
          decide(await _api.senderStatus());
        } catch (e) {
          debugPrint('sender status: ${describeError(e)}');
        }
      }

      unawaited(check());
      poll = Timer.periodic(pairingPollInterval, (_) => unawaited(check()));
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
      poll?.cancel();
      await sub.cancel();
      await _quietly(_api.senderStop);
      _pairingOnly = false;
    }
  }

  /// Stops streaming (and the Android capture service).
  Future<void> stop() async {
    if (state.busy) return;
    state = state.copyWith(busy: true);
    try {
      // Also when this controller did not start the capture (a capture
      // started by an earlier UI): stopping an idle capture is a no-op.
      if (_nativeCapture ||
          (ref.read(appInfoProvider).isAndroid &&
              state.source?.kind == SourceKind.deviceAudio)) {
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
