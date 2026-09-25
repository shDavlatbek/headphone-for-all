import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/hfa_api.dart';
import '../platform/native_channel.dart';
import 'core_providers.dart';

/// Hubs visible on the LAN.
@immutable
class DiscoveryState {
  /// Creates a state.
  const DiscoveryState({this.hubs = const {}, this.error});

  /// Visible hubs by device id, in discovery order.
  final Map<String, HubInfoDto> hubs;

  /// Why discovery stopped, if it failed.
  final String? error;
}

/// mDNS discovery; runs while something watches it (the sender screen).
final discoveryControllerProvider =
    NotifierProvider.autoDispose<DiscoveryController, DiscoveryState>(
      DiscoveryController.new,
    );

/// Browses for hubs with `discoverHubs`, holding the Android multicast lock
/// meanwhile, and stops when no longer watched.
class DiscoveryController extends Notifier<DiscoveryState> {
  StreamSubscription<DiscoveryEventDto>? _sub;

  @override
  DiscoveryState build() {
    final api = ref.watch(hfaApiProvider);
    final native = ref.watch(nativeChannelProvider);
    ref.onDispose(() {
      _sub?.cancel();
      _sub = null;
      unawaited(_stop(api, native));
    });
    unawaited(native.acquireMulticastLock().catchError(_logNative));
    _listen(api);
    return const DiscoveryState();
  }

  static Future<void> _stop(HfaApi api, NativeChannel native) async {
    await Future.wait([
      api.stopDiscovery().catchError((Object e) {
        debugPrint('stop discovery: ${describeError(e)}');
      }),
      native.releaseMulticastLock().catchError(_logNative),
    ]);
  }

  static void _logNative(Object e) {
    debugPrint('multicast lock: ${describeError(e)}');
  }

  void _listen(HfaApi api) {
    _sub?.cancel();
    _sub = api.discoverHubs().listen(
      _onEvent,
      onError: (Object e) {
        if (!ref.mounted) return;
        state = DiscoveryState(hubs: state.hubs, error: describeError(e));
      },
    );
  }

  /// Restarts browsing from an empty list.
  void refresh() {
    state = const DiscoveryState();
    _listen(ref.read(hfaApiProvider));
  }

  void _onEvent(DiscoveryEventDto event) {
    if (!ref.mounted) return;
    final hubs = Map.of(state.hubs);
    switch (event) {
      case DiscoveryEventDto_Found(field0: final hub):
        hubs[hub.deviceId] = hub;
      case DiscoveryEventDto_Lost(:final deviceId):
        hubs.remove(deviceId);
    }
    state = DiscoveryState(hubs: hubs);
  }
}
