import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/hfa_api.dart';
import 'core_providers.dart';
import 'pairing_controller.dart';
import 'settings_controller.dart';

/// What the hub screen shows.
@immutable
class HubState {
  /// Creates a state.
  const HubState({
    this.running = false,
    this.busy = false,
    this.port = 0,
    this.sources = const [],
    this.masterGain = 1,
    this.addresses = const [],
    this.advertised = false,
    this.advertiseError,
    this.error,
  });

  /// The hub runs.
  final bool running;

  /// A start or stop is in progress.
  final bool busy;

  /// Bound port while running.
  final int port;

  /// Incoming streams, in arrival order.
  final List<SourceDto> sources;

  /// Master linear gain (0..=4), as the core saved it (`HubStatusDto`).
  final double masterGain;

  /// Where senders can reach the running hub (`ip:port`, IPv4 first).
  final List<String> addresses;

  /// The running hub is announced over mDNS (senders find it by themselves).
  final bool advertised;

  /// Why announcing the running hub failed, if it did.
  final String? advertiseError;

  /// The hub runs but senders cannot find it: they must add it by address.
  bool get notDiscoverable => running && !advertised;

  /// The last error, kept until [HubController.clearError], a successful
  /// start or a stop: the app shell shows it in a snack bar wherever the
  /// user is, and the hub screen keeps showing it (e.g. after a failure from
  /// the desktop tray while the window was hidden).
  final String? error;

  /// A copy with the given fields replaced. [error] is kept unless passed
  /// (pass `null` to clear it).
  HubState copyWith({
    bool? running,
    bool? busy,
    int? port,
    List<SourceDto>? sources,
    double? masterGain,
    List<String>? addresses,
    bool? advertised,
    Object? advertiseError = _keep,
    Object? error = _keep,
  }) {
    return HubState(
      running: running ?? this.running,
      busy: busy ?? this.busy,
      port: port ?? this.port,
      sources: sources ?? this.sources,
      masterGain: masterGain ?? this.masterGain,
      addresses: addresses ?? this.addresses,
      advertised: advertised ?? this.advertised,
      advertiseError: identical(advertiseError, _keep)
          ? this.advertiseError
          : advertiseError as String?,
      error: identical(error, _keep) ? this.error : error as String?,
    );
  }

  /// A copy with what [status] reports (running hub or not: the master gain
  /// is the core's saved value either way).
  HubState withStatus(HubStatusDto status) => copyWith(
    running: status.running,
    port: status.port,
    masterGain: status.masterGain,
    addresses: status.addresses,
    advertised: status.advertised,
    advertiseError: status.advertiseError,
  );

  static const _keep = Object();
}

/// The hub: start/stop, live sources (events + polling) and mixer controls.
final hubControllerProvider = NotifierProvider<HubController, HubState>(
  HubController.new,
);

/// Drives the hub engine and keeps [HubState] current.
///
/// Sources arrive through `hubEvents` (added / updated about once per second /
/// removed) and are refreshed with `hubSources` every [pollIntervalProvider]
/// while the hub runs, so the list heals if an event was skipped; the status
/// (addresses, advertising) is refreshed with them.
///
/// The master gain lives in the core (`HubStatusDto.masterGain`, saved in
/// its settings and applied by every hub start), so the slider shows the
/// saved value from the first frame on, also while the hub is stopped.
class HubController extends Notifier<HubState> {
  StreamSubscription<HubEventDto>? _events;
  Timer? _poll;

  /// A master gain change is in flight: a status read meanwhile must not
  /// move the slider back.
  int _gainChanges = 0;

  HfaApi get _api => ref.read(hfaApiProvider);

  @override
  HubState build() {
    final api = ref.watch(hfaApiProvider);
    _events = api.hubEvents().listen(
      _onEvent,
      onError: (Object e) => _setError(describeError(e)),
    );
    ref.onDispose(() {
      _events?.cancel();
      _poll?.cancel();
    });
    // Pick up a hub that already runs (e.g. after a hot restart).
    Future.microtask(_syncStatus);
    return const HubState();
  }

  Future<void> _syncStatus() async {
    try {
      final status = await _api.hubStatus();
      if (!ref.mounted) return;
      if (_gainChanges == 0) {
        state = state.copyWith(masterGain: status.masterGain);
      }
      if (!status.running || state.running || state.busy) return;
      state = _applyStatus(status);
      _startPolling();
      await refreshSources();
    } catch (e) {
      debugPrint('hub status: ${describeError(e)}');
    }
  }

  /// [state] with [status] applied, keeping a master gain that is being set.
  HubState _applyStatus(HubStatusDto status) {
    final next = state.withStatus(status);
    return _gainChanges == 0
        ? next
        : next.copyWith(masterGain: state.masterGain);
  }

  /// Reloads the running hub's status (addresses change with the network).
  Future<void> refreshStatus() async {
    try {
      final status = await _api.hubStatus();
      if (!ref.mounted || !state.running || state.busy || !status.running) {
        return;
      }
      state = _applyStatus(status);
    } catch (e) {
      debugPrint('hub status: ${describeError(e)}');
    }
  }

  /// Starts the hub (and, on mobile, the native hub service first).
  Future<void> start() async {
    if (state.busy || state.running) return;
    // A new attempt: a repeated failure is reported again.
    state = state.copyWith(busy: true, error: null);
    final native = ref.read(nativeChannelProvider);
    final HubStatusDto status;
    try {
      await native.startHubService();
      status = await _api.hubStart();
    } catch (e) {
      // The core hub did not start: the service must not outlive it.
      await _quietly(native.stopHubService);
      if (!ref.mounted) return;
      state = state.copyWith(busy: false, error: describeError(e));
      return;
    }
    // The hub runs from here on (with the master gain the core saved).
    if (!ref.mounted) return;
    state = _applyStatus(status).copyWith(running: true, busy: false);
    _startPolling();
    await refreshSources();
  }

  /// Stops the hub and the native hub service.
  Future<void> stop() async {
    if (state.busy || !state.running) return;
    state = state.copyWith(busy: true);
    _poll?.cancel();
    _poll = null;
    ref.read(pairingControllerProvider.notifier).reset();
    String? error;
    try {
      await _api.hubStop();
    } catch (e) {
      error = describeError(e);
    }
    await _quietly(ref.read(nativeChannelProvider).stopHubService);
    if (!ref.mounted) return;
    state = HubState(masterGain: state.masterGain, error: error);
  }

  /// Starts or stops the hub.
  Future<void> toggle() => state.running ? stop() : start();

  /// Restarts a running hub (e.g. so it reloads its trust store).
  Future<void> restart() async {
    if (!state.running) return;
    await stop();
    await start();
  }

  /// Reloads the sources from the core.
  Future<void> refreshSources() async {
    try {
      final sources = await _api.hubSources();
      if (!ref.mounted || !state.running) return;
      state = state.copyWith(sources: _merge(state.sources, sources));
    } catch (e) {
      debugPrint('hub sources: ${describeError(e)}');
    }
  }

  /// Keeps the previous order and appends new streams.
  static List<SourceDto> _merge(List<SourceDto> old, List<SourceDto> fresh) {
    final byId = {for (final s in fresh) s.streamId: s};
    return [for (final s in old) ?byId.remove(s.streamId), ...byId.values];
  }

  void _startPolling() {
    _poll?.cancel();
    _poll = Timer.periodic(ref.read(pollIntervalProvider), (_) {
      refreshSources();
      refreshStatus();
    });
  }

  /// Sets a stream's gain (optimistic; reverted if the core refuses).
  Future<void> setGain(int streamId, double gain) =>
      _control(streamId, (s) => _with(s, gain: gain), () {
        return _api.hubSetGain(streamId, gain);
      });

  /// Mutes or unmutes a stream.
  Future<void> setMuted(int streamId, bool muted) =>
      _control(streamId, (s) => _with(s, muted: muted), () {
        return _api.hubSetMuted(streamId, muted);
      });

  /// Marks a stream as priority or not.
  Future<void> setPriority(int streamId, bool priority) =>
      _control(streamId, (s) => _with(s, priority: priority), () {
        return _api.hubSetPriority(streamId, priority);
      });

  /// Shows [gain] on the master slider while it is dragged (no core call).
  void previewMasterGain(double gain) {
    state = state.copyWith(masterGain: gain);
  }

  /// Sets the master gain. The core saves it (also while the hub is
  /// stopped) and every hub start applies it; if the core refuses, the
  /// slider goes back to the saved value.
  Future<void> setMasterGain(double gain) async {
    _gainChanges++;
    state = state.copyWith(masterGain: gain);
    try {
      await _api.hubSetMasterGain(gain);
    } catch (e) {
      double? saved;
      try {
        saved = (await _api.hubStatus()).masterGain;
      } catch (e) {
        debugPrint('hub status: ${describeError(e)}');
      }
      if (!ref.mounted) return;
      state = state.copyWith(masterGain: saved, error: describeError(e));
    } finally {
      _gainChanges--;
    }
  }

  /// Forgets the current error.
  void clearError() {
    if (state.error != null) state = state.copyWith(error: null);
  }

  Future<void> _control(
    int streamId,
    SourceDto Function(SourceDto) change,
    Future<void> Function() call,
  ) async {
    final before = state.sources;
    state = state.copyWith(
      sources: [for (final s in before) s.streamId == streamId ? change(s) : s],
    );
    try {
      await call();
    } catch (e) {
      if (!ref.mounted) return;
      state = state.copyWith(sources: before, error: describeError(e));
    }
  }

  void _onEvent(HubEventDto event) {
    if (!ref.mounted) return;
    switch (event) {
      case HubEventDto_SourceAdded(field0: final source) ||
          HubEventDto_SourceUpdated(field0: final source):
        _upsert(source);
      case HubEventDto_SourceRemoved(:final streamId):
        state = state.copyWith(
          sources: [
            for (final s in state.sources)
              if (s.streamId != streamId) s,
          ],
        );
      case HubEventDto_PairingCompleted(:final deviceId, :final name):
        ref
            .read(pairingControllerProvider.notifier)
            .onCompleted(deviceId: deviceId, name: name);
        ref.invalidate(trustedPeersProvider);
      case HubEventDto_PairingFailed(:final reason):
        ref.read(pairingControllerProvider.notifier).onFailed(reason);
      case HubEventDto_Error(:final message):
        _setError(message);
    }
  }

  void _upsert(SourceDto source) {
    if (!state.running) return;
    final sources = state.sources;
    final index = sources.indexWhere((s) => s.streamId == source.streamId);
    state = state.copyWith(
      sources: index < 0
          ? [...sources, source]
          : [
              for (var i = 0; i < sources.length; i++)
                i == index ? source : sources[i],
            ],
    );
  }

  void _setError(String message) {
    if (!ref.mounted) return;
    state = state.copyWith(error: message);
  }

  static Future<void> _quietly(Future<void> Function() action) async {
    try {
      await action();
    } catch (e) {
      debugPrint('native hub service: ${describeError(e)}');
    }
  }

  static SourceDto _with(
    SourceDto s, {
    double? gain,
    bool? muted,
    bool? priority,
  }) {
    return SourceDto(
      streamId: s.streamId,
      deviceId: s.deviceId,
      deviceName: s.deviceName,
      label: s.label,
      platform: s.platform,
      gain: gain ?? s.gain,
      muted: muted ?? s.muted,
      priority: priority ?? s.priority,
      active: s.active,
      lossPct: s.lossPct,
      jitterMs: s.jitterMs,
      bufferMs: s.bufferMs,
      latencyMs: s.latencyMs,
      levelDb: s.levelDb,
    );
  }
}
