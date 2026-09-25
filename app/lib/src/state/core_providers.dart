/// Providers every other provider builds on: the core API, the native
/// channel, the device info and a few injectable knobs (clock, poll interval).
library;

import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/hfa_api.dart';
import '../platform/native_channel.dart';

/// The core API. Overridden at startup with [RustHfaApi] or [FakeHfaApi].
final hfaApiProvider = Provider<HfaApi>(
  (ref) => throw UnimplementedError('hfaApiProvider must be overridden'),
);

/// The native platform channel (no-op on Windows and Linux).
final nativeChannelProvider = Provider<NativeChannel>((ref) => NativeChannel());

/// The [AppInfo] returned by `initApp` at startup. Must be overridden.
final initialAppInfoProvider = Provider<AppInfo>(
  (ref) =>
      throw UnimplementedError('initialAppInfoProvider must be overridden'),
);

/// The data directory passed to `initApp`, for files the app keeps next to
/// the core's (`null` in tests and the demo mode: nothing is written).
final dataDirProvider = Provider<String?>((ref) => null);

/// Whether the app runs against [FakeHfaApi] (`--dart-define=HFA_FAKE=true`).
final demoModeProvider = Provider<bool>((ref) => false);

/// The current time (overridable in tests).
final clockProvider = Provider<DateTime Function()>((ref) => DateTime.now);

/// How often the hub screen refreshes its sources with `hubSources`.
final pollIntervalProvider = Provider<Duration>(
  (ref) => const Duration(seconds: 1),
);

/// This device's info; the name follows the settings.
final appInfoProvider = NotifierProvider<AppInfoNotifier, AppInfo>(
  AppInfoNotifier.new,
);

/// Holds the current [AppInfo].
class AppInfoNotifier extends Notifier<AppInfo> {
  @override
  AppInfo build() => ref.watch(initialAppInfoProvider);

  /// Updates the device name after the settings changed.
  void rename(String deviceName) {
    final info = state;
    state = AppInfo(
      deviceId: info.deviceId,
      deviceName: deviceName,
      platform: info.platform,
      version: info.version,
      capabilities: info.capabilities,
    );
  }
}

/// Platform helpers on [AppInfo.platform] (the core's platform string).
extension AppPlatform on AppInfo {
  /// Running on Android.
  bool get isAndroid => platform == 'android';

  /// Running on iOS.
  bool get isIos => platform == 'ios';

  /// Running on a phone or tablet.
  bool get isMobile => isAndroid || isIos;

  /// Running on Windows, macOS or Linux.
  bool get isDesktop =>
      platform == 'windows' || platform == 'macos' || platform == 'linux';
}
