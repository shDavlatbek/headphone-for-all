/// Startup: loads the Rust library, finds the data directory and calls
/// `initApp`; plus the screens shown while that runs or when it fails.
library;

import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:path_provider/path_provider.dart';

import 'api/hfa_api.dart';
import 'app.dart';
import 'platform/native_channel.dart';
import 'rust/frb_generated.dart';

bool _rustLoaded = false;

/// Loads the Rust library once (`RustLib.init()`).
Future<void> loadRustLibrary() async {
  if (_rustLoaded) return;
  await RustLib.init();
  _rustLoaded = true;
}

/// The data directory: the native one where the platform provides it
/// (Android `filesDir/hfa`, the iOS App Group container shared with the
/// broadcast extension, macOS Application Support), else
/// `<application support>/hfa` from path_provider.
Future<String> resolveDataDir(NativeChannel native) async {
  String? nativeDir;
  try {
    nativeDir = await native.getDataDir();
  } on PlatformException catch (e) {
    debugPrint('getDataDir failed, using path_provider: ${e.message}');
  }
  if (nativeDir != null && nativeDir.isNotEmpty) return nativeDir;
  final support = await getApplicationSupportDirectory();
  return '${support.path}${Platform.pathSeparator}hfa';
}

/// The device name used on the very first run. Desktop: `null` (the core
/// uses the host name). Mobile host names are often just `localhost`, so a
/// generic name is used then (the user can rename the device in Settings).
String? firstRunDeviceName() {
  if (kIsWeb) return null;
  if (!Platform.isAndroid && !Platform.isIOS) return null;
  final host = Platform.localHostname.trim();
  if (host.isNotEmpty && host.toLowerCase() != 'localhost') return host;
  return Platform.isAndroid ? 'Android device' : 'iOS device';
}

/// The core platform string of the host (used by the demo mode's fake).
String hostPlatformName() {
  if (kIsWeb) return 'unknown';
  if (Platform.isAndroid) return 'android';
  if (Platform.isIOS) return 'ios';
  if (Platform.isMacOS) return 'macos';
  if (Platform.isWindows) return 'windows';
  if (Platform.isLinux) return 'linux';
  return 'unknown';
}

/// Initializes the core: `RustLib.init()` (unless [loadRust] is false, as in
/// the demo mode) → data directory → `initApp`.
Future<AppInfo> initCore({
  required HfaApi api,
  required NativeChannel native,
  bool loadRust = true,
}) async {
  if (loadRust) await loadRustLibrary();
  final dataDir = await resolveDataDir(native);
  return api.initApp(dataDir: dataDir, deviceName: firstRunDeviceName());
}

/// Shown while the core starts.
class SplashApp extends StatelessWidget {
  /// Creates the splash.
  const SplashApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: appTitle,
      debugShowCheckedModeBanner: false,
      theme: lightTheme(),
      darkTheme: darkTheme(),
      home: const Scaffold(
        body: Center(
          child: Column(
            mainAxisSize: MainAxisSize.min,
            children: [
              Icon(Icons.headphones, size: 64),
              SizedBox(height: 24),
              CircularProgressIndicator(),
            ],
          ),
        ),
      ),
    );
  }
}

/// A friendly error screen for a failed startup, with "Try again".
class InitErrorApp extends StatelessWidget {
  /// Creates the screen for [error]; [onRetry] restarts initialization.
  const InitErrorApp({super.key, required this.error, this.onRetry});

  /// What went wrong.
  final String error;

  /// Starts again; hidden when `null`.
  final VoidCallback? onRetry;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: appTitle,
      debugShowCheckedModeBanner: false,
      theme: lightTheme(),
      darkTheme: darkTheme(),
      home: Builder(
        builder: (context) {
          final theme = Theme.of(context);
          return Scaffold(
            body: SafeArea(
              child: Center(
                child: SingleChildScrollView(
                  padding: const EdgeInsets.all(24),
                  child: ConstrainedBox(
                    constraints: const BoxConstraints(maxWidth: 480),
                    child: Column(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        Icon(
                          Icons.headset_off_outlined,
                          size: 64,
                          color: theme.colorScheme.error,
                        ),
                        const SizedBox(height: 16),
                        Text(
                          "Headphone for All couldn't start",
                          style: theme.textTheme.headlineSmall,
                          textAlign: TextAlign.center,
                        ),
                        const SizedBox(height: 12),
                        const Text(
                          'The audio engine failed to load. Restarting the '
                          'app usually helps; if it keeps happening, please '
                          'report the details below.',
                          textAlign: TextAlign.center,
                        ),
                        const SizedBox(height: 16),
                        Card(
                          color: theme.colorScheme.surfaceContainerHighest,
                          child: Padding(
                            padding: const EdgeInsets.all(12),
                            child: SelectableText(
                              error,
                              key: const Key('init-error'),
                              style: theme.textTheme.bodySmall?.copyWith(
                                fontFamily: 'monospace',
                              ),
                            ),
                          ),
                        ),
                        if (onRetry != null) ...[
                          const SizedBox(height: 16),
                          FilledButton.icon(
                            icon: const Icon(Icons.refresh),
                            label: const Text('Try again'),
                            onPressed: onRetry,
                          ),
                        ],
                      ],
                    ),
                  ),
                ),
              ),
            ),
          );
        },
      ),
    );
  }
}
