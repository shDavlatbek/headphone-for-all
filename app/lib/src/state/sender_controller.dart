import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart' show PlatformException;
import 'package:flutter/widgets.dart' show AppLifecycleListener;
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/hfa_api.dart';
import '../models/hub_target.dart';
import '../models/source_choice.dart';
import '../platform/native_channel.dart';
import 'app_prefs.dart';
import 'core_providers.dart';
import 'hub_address_book.dart';
import 'settings_controller.dart';

/// Sender states in which a sender is live (`sender_start` would refuse).
const liveSenderStates = {'connecting', 'pairing', 'streaming', 'reconnecting'};

/// Why a hub without an address cannot be used on iOS.
const iosNeedsHubAddress =
    'This device cannot look for hubs on the network. Add the hub by '
    "address or scan its QR code (on the hub: Pair a device).";

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
    this.broadcast = BroadcastStatus.idle,
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

  /// iOS: what the broadcast extension last reported (`getBroadcastStatus`,
  /// `broadcastStatus` events); [BroadcastStatus.idle] when nothing is known.
  final BroadcastStatus broadcast;

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
    BroadcastStatus? broadcast,
  }) {
    return SenderState(
      status: status ?? this.status,
      target: clearTarget ? null : (target ?? this.target),
      source: source ?? this.source,
      busy: busy ?? this.busy,
      error: error,
      broadcastReady: broadcastReady ?? this.broadcastReady,
      broadcasting: broadcasting ?? this.broadcasting,
      broadcast: broadcast ?? this.broadcast,
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

  /// Android: a native capture may run for the core's sender. Set while its
  /// start is pending, while it records, and when a live sender is adopted
  /// from an earlier UI; cleared once `stopSystemCapture` was called.
  bool _nativeCapture = false;

  /// Android: `startSystemCapture` is in flight.
  bool _capturePending = false;

  /// Android: the reason of a `captureError` that arrived while the start
  /// was pending (the start then answers `false`, §8.8).
  String? _pendingCaptureError;

  /// The first status of the subscription (the core's status before this
  /// controller did anything) has not arrived yet.
  bool _awaitingFirstStatus = true;

  /// [start] ran in this controller.
  bool _startedHere = false;

  /// Android: a capture found without a live sender at startup was stopped
  /// (by the first status or by the native capture status, whichever came
  /// first).
  bool _strayCaptureStopped = false;

  /// This sender holds a reference on the multicast lock (it finds its hub
  /// by id over mDNS, also on every reconnect).
  bool _holdsMulticast = false;

  /// iOS pairing run in progress (its target is updated when it ends).
  bool _pairingOnly = false;

  /// macOS: this sender holds the App Nap opt-out (`beginStreaming`).
  bool _holdsActivity = false;

  /// The user picked a hub or a source, or a start ran: the remembered ones
  /// are no longer preselected.
  bool _chosen = false;

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
    final info = ref.read(appInfoProvider);
    AppLifecycleListener? lifecycle;
    if (info.isIos) {
      // The extension runs while the app is in the background; what it did
      // meanwhile is asked for again when the app comes back.
      lifecycle = AppLifecycleListener(
        onResume: () => unawaited(refreshBroadcastStatus()),
      );
      Future.microtask(refreshBroadcastStatus);
    }
    ref.onDispose(() {
      _statusSub?.cancel();
      _nativeSub?.cancel();
      lifecycle?.dispose();
      _releaseMulticast(native);
      _endActivity(native);
    });
    // The hub and source this device last sent with, once the prefs loaded.
    ref.listen(appPrefsProvider, (_, prefs) => _preselect(prefs));
    Future.microtask(() {
      if (ref.mounted) _preselect(ref.read(appPrefsProvider));
    });
    final sources = availableSources(info);
    if (info.isAndroid) unawaited(_syncNativeCapture());
    return SenderState(
      status: idleSenderStatus,
      source: sources.isEmpty ? null : SourceChoice(sources.first),
    );
  }

  /// Preselects the remembered hub and source (see [AppPrefs.lastHub]) until
  /// the user chooses or starts something.
  void _preselect(AppPrefs prefs) {
    final hub = prefs.lastHub;
    if (_chosen || _startedHere || hub == null) return;
    if (state.target != null || state.busy || state.isLive) return;
    final info = ref.read(appInfoProvider);
    // Not this device's own hub (e.g. prefs copied from another device).
    if (hub.deviceId == info.deviceId) return;
    final source = prefs.lastSource;
    final available = availableSources(info);
    state = state.copyWith(
      target: hub.toTarget(trusted: _trustedAsHub(hub.deviceId)),
      source: source != null && available.contains(source.kind)
          ? source.toChoice()
          : null,
    );
    if (source?.kind == SourceKind.app && source?.appName != null) {
      unawaited(_findRememberedApp(source!, preselected: true));
    }
    // The trust store decides whether a PIN is needed.
    unawaited(_refreshRememberedTrust(hub));
  }

  /// Whether the loaded trust store has [deviceId] as a hub.
  bool _trustedAsHub(String? deviceId) {
    if (deviceId == null) return false;
    final peers = ref.read(trustedPeersProvider).value ?? const [];
    return peers.any((p) => p.deviceId == deviceId && p.pairedAsHub);
  }

  Future<void> _refreshRememberedTrust(LastHub hub) async {
    final id = hub.deviceId;
    if (id == null) return;
    try {
      final peers = await ref.read(trustedPeersProvider.future);
      if (!ref.mounted || _chosen || _startedHere) return;
      final target = state.target;
      if (target == null || target.deviceId != id) return;
      final trusted = peers.any((p) => p.deviceId == id && p.pairedAsHub);
      if (trusted != target.trusted) {
        state = state.copyWith(target: hub.toTarget(trusted: trusted));
      }
    } catch (e) {
      debugPrint('trusted peers: ${describeError(e)}');
    }
  }

  /// Selects the remembered app again if it runs now. [preselected]: the
  /// app is only preselected, so a choice the user made meanwhile wins.
  Future<void> _findRememberedApp(
    LastSource source, {
    required bool preselected,
  }) async {
    try {
      final apps = await ref.read(hfaApiProvider).listCaptureApps();
      if (!ref.mounted || state.busy || state.isLive) return;
      if (preselected && (_chosen || _startedHere)) return;
      final choice = source.toChoice(apps);
      if (choice.app != null && state.source?.kind == SourceKind.app) {
        state = state.copyWith(source: choice);
      }
    } catch (e) {
      debugPrint('capture apps: ${describeError(e)}');
    }
  }

  /// Selects the hub and source this device last sent with ("Send to …" on
  /// the home screen), trusted as the trust store says. Returns `false` when
  /// nothing is remembered or a sender is live.
  Future<bool> useRemembered() async {
    final prefs = ref.read(appPrefsProvider);
    final hub = prefs.lastHub;
    if (hub == null || state.isLive || state.busy) return false;
    _chosen = true;
    var trusted = false;
    final id = hub.deviceId;
    if (id != null) {
      try {
        final peers = await ref.read(trustedPeersProvider.future);
        trusted = peers.any((p) => p.deviceId == id && p.pairedAsHub);
      } catch (e) {
        debugPrint('trusted peers: ${describeError(e)}');
      }
    }
    if (!ref.mounted || state.isLive || state.busy) return false;
    final source = prefs.lastSource;
    final available = availableSources(ref.read(appInfoProvider));
    state = state.copyWith(
      target: hub.toTarget(trusted: trusted),
      source: source != null && available.contains(source.kind)
          ? source.toChoice()
          : null,
      broadcastReady: false,
    );
    if (source?.kind == SourceKind.app && source?.appName != null) {
      await _findRememberedApp(source!, preselected: false);
    }
    return ref.mounted;
  }

  /// iOS: asks the extension's state again (`getBroadcastStatus`). A no-op
  /// where the platform does not report one.
  Future<void> refreshBroadcastStatus() async {
    BroadcastStatus? status;
    try {
      status = await _native.getBroadcastStatus();
    } catch (e) {
      debugPrint('broadcast status: ${describeError(e)}');
    }
    if (status == null || !ref.mounted) return;
    _applyBroadcast(status);
  }

  void _applyBroadcast(BroadcastStatus status) {
    state = state.copyWith(
      broadcast: status,
      broadcasting: status.state.isRunning,
      error: status.state == BroadcastState.failed
          ? (status.message ?? 'The broadcast failed.')
          : state.error,
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
    // A start in this controller owns the capture from then on.
    if (native == null || !ref.mounted || state.busy || _startedHere) return;
    if (!native.running && native.endedWhileAway == null) return;
    SenderStatusDto? now;
    try {
      now = await _api.senderStatus();
    } catch (e) {
      debugPrint('sender status: ${describeError(e)}');
    }
    if (now == null || !ref.mounted || state.busy || _startedHere) return;
    final live = liveSenderStates.contains(now.state);
    if (native.running) {
      if (live) {
        _nativeCapture = true;
      } else {
        await _stopStrayCapture();
      }
    } else if (live) {
      // The first status may have adopted this sender's capture already.
      _nativeCapture = false;
      final why = native.endedWhileAway!;
      state = state.copyWith(
        error:
            'Capture stopped while the app was closed'
            '${why.isEmpty ? '' : ': $why'}',
      );
      await _quietly(_api.senderStop);
    }
  }

  bool get _isAndroid => ref.read(appInfoProvider).isAndroid;

  void _onStatus(SenderStatusDto status) {
    if (!ref.mounted) return;
    final first = _awaitingFirstStatus;
    _awaitingFirstStatus = false;
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
    // Offered again next time ("Send to …", preselected).
    final source = state.source;
    if (status.state == 'streaming' &&
        target != null &&
        source != null &&
        !_pairingOnly &&
        _startedHere) {
      ref
          .read(appPrefsProvider.notifier)
          .rememberSend(paired ?? target, source);
    }
    // The core saved the hub as trusted: refresh the lists that show it.
    if (paired != null) ref.invalidate(trustedPeersProvider);
    // Where the hub was reached: iOS cannot find it by id later (§8.9).
    final hubId = target?.deviceId;
    if (status.state == 'streaming' && target != null && hubId != null) {
      ref
          .read(hubAddressBookProvider.notifier)
          .remember(hubId, target.host, target.port);
    }
    final live = liveSenderStates.contains(status.state);
    if (first) {
      // The status from before this controller acted.
      if (!_startedHere && _isAndroid) _adoptAndroidCapture(live);
      return;
    }
    if (!live) {
      _releaseMulticast(_native);
      _endActivity(_native);
      if (_nativeCapture) {
        // The engine ended by itself (failed / stopped), possibly while the
        // consent dialog was open: stop (or cancel) the capture too.
        _nativeCapture = false;
        unawaited(_quietly(_native.stopSystemCapture));
      }
    }
  }

  /// Android: the core's sender and `CaptureService` live as long as the
  /// process, this controller only as long as the Flutter engine (§8.8), e.g.
  /// the task was swiped away while sending and the app was opened again.
  /// A live sender found at startup may therefore have a capture that this
  /// controller must stop with it; with no live sender, a capture left
  /// behind would record for nobody, so it is stopped now.
  /// (`stopSystemCapture` is idempotent and emits no event.)
  void _adoptAndroidCapture(bool live) {
    if (live) {
      _nativeCapture = true;
    } else {
      unawaited(_stopStrayCapture());
    }
  }

  /// Stops a capture left behind by an earlier UI, once.
  Future<void> _stopStrayCapture() async {
    if (_strayCaptureStopped) return;
    _strayCaptureStopped = true;
    await _quietly(_native.stopSystemCapture);
  }

  void _onNativeEvent(NativeEvent event) {
    if (!ref.mounted) return;
    switch (event.type) {
      case NativeEventType.captureStopped:
      case NativeEventType.captureError:
        if (_capturePending) {
          // The start answers `false`; [_startAndroid] explains why.
          if (event.type == NativeEventType.captureError) {
            _pendingCaptureError = event.message ?? 'unknown error';
          }
          return;
        }
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
        state = state.copyWith(
          broadcasting: false,
          error: event.message,
          broadcast: BroadcastStatus(
            state: BroadcastState.stopped,
            message: event.message,
            hubName: state.broadcast.hubName,
          ),
        );
      case NativeEventType.broadcastStatus:
        final status = event.broadcast;
        if (status != null) _applyBroadcast(status);
      case NativeEventType.unknown:
        break;
    }
  }

  /// Selects the hub to stream to.
  void selectTarget(HubTarget? target) {
    _chosen = true;
    state = target == null
        ? state.copyWith(clearTarget: true, broadcastReady: false)
        : state.copyWith(target: target, broadcastReady: false);
  }

  /// Selects what to capture.
  void selectSource(SourceChoice source) {
    _chosen = true;
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
    if (target.host.isEmpty && ref.read(appInfoProvider).isIos) {
      // No mDNS on iOS (§8.9): neither the app's sender nor the broadcast
      // extension could find the hub by its id.
      state = state.copyWith(error: iosNeedsHubAddress);
      return;
    }
    state = state.copyWith(busy: true);
    _startedHere = true;
    // Found by id over mDNS (now and on every reconnect): Android filters
    // multicast without the lock, also after the sender screen is left.
    if (target.host.isEmpty && source.kind != SourceKind.broadcast) {
      _acquireMulticast();
    }
    try {
      switch (source.kind) {
        case SourceKind.deviceAudio:
          await _startAndroid(target, source);
        case SourceKind.broadcast:
          await _prepareBroadcast(target, source);
        default:
          await _beginActivity();
          await _api.senderStart(
            target.toRequest(source.toDto(), label: source.label),
          );
      }
      if (!ref.mounted) return;
      state = state.copyWith(busy: false);
    } catch (e) {
      _releaseMulticast(_native);
      _endActivity(_native);
      if (!ref.mounted) return;
      state = state.copyWith(busy: false, error: describeError(e));
    }
  }

  /// macOS: keeps App Nap away while this device sends (a Mac that sends
  /// usually hides its window). Only macOS implements it (CONTRACTS §8.9).
  Future<void> _beginActivity() async {
    if (_holdsActivity || ref.read(appInfoProvider).platform != 'macos') {
      return;
    }
    _holdsActivity = true;
    await _quietly(_native.beginStreaming);
  }

  void _endActivity(NativeChannel native) {
    if (!_holdsActivity) return;
    _holdsActivity = false;
    unawaited(_quietly(native.endStreaming));
  }

  Future<void> _startAndroid(HubTarget target, SourceChoice source) async {
    await _api.senderStart(
      target.toRequest(source.toDto(), label: source.label),
    );
    // The consent dialog can stay open for a while; the sender may fail
    // meanwhile (wrong PIN, pairing required, key mismatch). Its capture
    // counts from now on, so such a failure stops (cancels) it too, and
    // capture events are honoured.
    _nativeCapture = true;
    _capturePending = true;
    _pendingCaptureError = null;
    final bool started;
    try {
      started = await _native.startSystemCapture(
        feedId: androidFeedId,
        sampleRate: nativeSampleRate,
        channels: nativeChannels,
      );
    } on PlatformException catch (e) {
      // `permissionDenied` (RECORD_AUDIO refused) or `invalidArgument`.
      _nativeCapture = false;
      await _quietly(_api.senderStop);
      throw HfaApiException(e.message ?? 'Audio capture failed (${e.code}).');
    } catch (e) {
      _nativeCapture = false;
      await _quietly(_api.senderStop);
      rethrow;
    } finally {
      _capturePending = false;
    }
    final captureError = _pendingCaptureError;
    _pendingCaptureError = null;
    if (!_nativeCapture) {
      // The sender ended while the dialog was open; its capture was stopped
      // (the start cancelled) then. Its reason is what matters.
      throw HfaApiException(
        (ref.mounted ? state.status.error : null) ?? 'The sender stopped.',
      );
    }
    if (!started) {
      _nativeCapture = false;
      await _quietly(_api.senderStop);
      throw HfaApiException(
        captureError != null
            ? 'Audio capture could not start: $captureError'
            : 'Audio capture was not allowed. Tap Start again and accept the '
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
      if (_nativeCapture) {
        _nativeCapture = false;
        await _quietly(_native.stopSystemCapture);
      }
      throw HfaApiException(
        now?.error ?? state.status.error ?? 'The sender stopped.',
      );
    }
  }

  /// iOS: makes sure the hub trusts us (pairing through a short-lived
  /// sender when a secret is at hand), then hands the hub to the extension.
  ///
  /// The extension never pairs and needs the hub's key or a trusted device
  /// id (CONTRACTS.md §8.5), so a hub typed in by address is identified
  /// after pairing by the device the pairing added to the trust store.
  ///
  /// Without a secret the hub must be in the trust store as a hub: its id
  /// (or its key's fingerprint) is looked up there. A key alone (e.g. kept
  /// from a QR code of a hub that was forgotten since) proves nothing, so
  /// it asks for a PIN instead of writing a config the extension could not
  /// use.
  Future<void> _prepareBroadcast(HubTarget target, SourceChoice source) async {
    var deviceId = target.deviceId;
    if (target.pairingSecret == null &&
        !await _pairedAsHub(deviceId, target.hubKey)) {
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
    if (deviceId != null) {
      ref
          .read(hubAddressBookProvider.notifier)
          .remember(deviceId, target.host, target.port);
    }
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
    final ready = target.asPaired(deviceId: deviceId);
    state = state.copyWith(target: ready, broadcastReady: true);
    // Offered again next time ("Send to …", preselected).
    ref.read(appPrefsProvider.notifier).rememberSend(ready, source);
  }

  /// Whether the trust store holds the hub [deviceId] (else the device of
  /// [hubKey]) as a hub this device paired with. Asks the core, not the
  /// target's (possibly stale) `trusted` flag.
  Future<bool> _pairedAsHub(String? deviceId, String? hubKey) async {
    String? id = deviceId;
    if (hubKey != null) {
      final String keyId;
      try {
        keyId = await _api.fingerprintOfKey(hubKey);
      } catch (e) {
        debugPrint('hub key: ${describeError(e)}');
        return false;
      }
      // A key that is not this device id's is no identification at all.
      if (id != null && id != keyId) return false;
      id = keyId;
    }
    if (id == null) return false;
    final peers = await _api.trustedPeers();
    return peers.any((p) => p.deviceId == id && p.pairedAsHub);
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
          hubGain: 1,
          hubMuted: false,
          hubPriority: false,
        ),
      );
    } finally {
      poll?.cancel();
      await sub.cancel();
      await _quietly(_api.senderStop);
      _pairingOnly = false;
    }
  }

  /// Stops a live sender and starts it again with the same hub and source
  /// (e.g. so it picks up new settings). Android asks for consent again.
  Future<void> restart() async {
    if (!state.isLive) return;
    await stop();
    if (!ref.mounted || state.error != null) return;
    await start();
  }

  /// Stops streaming (and the Android capture service).
  Future<void> stop() async {
    if (state.busy) return;
    state = state.copyWith(busy: true);
    _endActivity(_native);
    try {
      // On Android always: a capture may run that this controller did not
      // start (§8.8: idempotent, no event).
      if (_nativeCapture || _isAndroid) {
        _nativeCapture = false;
        await _quietly(_native.stopSystemCapture);
      }
      _releaseMulticast(_native);
      await _api.senderStop();
      if (!ref.mounted) return;
      state = state.copyWith(busy: false);
    } catch (e) {
      if (!ref.mounted) return;
      state = state.copyWith(busy: false, error: describeError(e));
    }
  }

  void _acquireMulticast() {
    if (_holdsMulticast) return;
    _holdsMulticast = true;
    unawaited(_quietly(_native.acquireMulticastLock));
  }

  void _releaseMulticast(NativeChannel native) {
    if (!_holdsMulticast) return;
    _holdsMulticast = false;
    unawaited(_quietly(native.releaseMulticastLock));
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
