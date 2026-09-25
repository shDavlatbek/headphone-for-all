// Entry point: WidgetsFlutterBinding → RustLib.init() (skipped in the demo
// mode) → data directory → initApp → runApp(ProviderScope(HfaApp)).
//
// Demo mode without the Rust core: flutter run --dart-define=HFA_FAKE=true

import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import 'src/api/hfa_api.dart';
import 'src/app.dart';
import 'src/bootstrap.dart';
import 'src/platform/desktop_integration.dart';
import 'src/platform/native_channel.dart';
import 'src/state/core_providers.dart';

/// Runs against [FakeHfaApi] instead of the Rust core.
const demoMode = bool.fromEnvironment('HFA_FAKE');

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  try {
    await initDesktopWindow();
  } catch (e) {
    debugPrint('window manager: $e');
  }
  await start();
}

/// Initializes the core and shows the app, or the error screen.
Future<void> start() async {
  runApp(const SplashApp());
  final native = NativeChannel();
  final HfaApi api = demoMode
      ? FakeHfaApi.demo(platform: hostPlatformName())
      : const RustHfaApi();
  final AppInfo info;
  final String dataDir;
  try {
    (:info, :dataDir) = await initCore(
      api: api,
      native: native,
      loadRust: !demoMode,
    );
  } catch (e, stack) {
    debugPrint('startup failed: $e\n$stack');
    runApp(InitErrorApp(error: describeError(e), onRetry: start));
    return;
  }
  runApp(
    ProviderScope(
      // Errors are shown to the user; never retry engine calls silently.
      retry: (retryCount, error) => null,
      overrides: [
        hfaApiProvider.overrideWithValue(api),
        nativeChannelProvider.overrideWithValue(native),
        initialAppInfoProvider.overrideWithValue(info),
        demoModeProvider.overrideWithValue(demoMode),
        // The demo mode writes nothing next to a real core's files.
        dataDirProvider.overrideWithValue(demoMode ? null : dataDir),
      ],
      child: DesktopIntegration(enabled: isDesktopHost, child: const HfaApp()),
    ),
  );
}
