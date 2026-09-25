import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/hfa_api.dart';
import 'core_providers.dart';

/// The saved settings.
final settingsControllerProvider =
    AsyncNotifierProvider<SettingsController, SettingsDto>(
      SettingsController.new,
    );

/// Loads and saves [SettingsDto].
class SettingsController extends AsyncNotifier<SettingsDto> {
  @override
  Future<SettingsDto> build() => ref.watch(hfaApiProvider).getSettings();

  /// Validates and saves [settings] in the core. Throws the core's validation
  /// error (the previous settings stay).
  Future<void> save(SettingsDto settings) async {
    await ref.read(hfaApiProvider).updateSettings(settings);
    final saved = await ref.read(hfaApiProvider).getSettings();
    if (!ref.mounted) return;
    state = AsyncData(saved);
    ref.read(appInfoProvider.notifier).rename(saved.deviceName);
  }
}

/// Paired devices.
final trustedPeersProvider =
    AsyncNotifierProvider<TrustedPeersController, List<TrustedPeerDto>>(
      TrustedPeersController.new,
    );

/// Loads the trust store and forgets devices.
class TrustedPeersController extends AsyncNotifier<List<TrustedPeerDto>> {
  @override
  Future<List<TrustedPeerDto>> build() =>
      ref.watch(hfaApiProvider).trustedPeers();

  /// Removes [deviceId] from the trust store and reloads the list.
  Future<void> forget(String deviceId) async {
    final api = ref.read(hfaApiProvider);
    await api.forgetPeer(deviceId);
    final peers = await api.trustedPeers();
    if (!ref.mounted) return;
    state = AsyncData(peers);
  }
}

/// Output devices for the hub (desktop).
final outputDevicesProvider = FutureProvider.autoDispose<List<String>>(
  (ref) => ref.watch(hfaApiProvider).listOutputDevices(),
);

/// Apps that can be captured on their own.
final captureAppsProvider = FutureProvider.autoDispose<List<CaptureAppDto>>(
  (ref) => ref.watch(hfaApiProvider).listCaptureApps(),
);
