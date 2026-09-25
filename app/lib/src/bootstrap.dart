/// Startup: loads the Rust library, finds the data directory and calls
/// `initApp`; plus the screens shown while that runs or when it fails.
library;

import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart'
    show ExternalLibrary;
import 'package:path_provider/path_provider.dart';

import 'api/hfa_api.dart';
import 'app.dart';
import 'platform/native_channel.dart';
import 'rust/frb_generated.dart';

bool _rustLoaded = false;

/// The CocoaPods framework that carries the Rust core on iOS and macOS.
///
/// cargokit builds `libhfa_ffi.a` and force-loads it into the pod
/// `rust_lib_headphone_for_all` (`rust_builder/{ios,macos}/*.podspec`), and
/// Flutter's Podfile uses `use_frameworks!`, so the symbols live in this
/// framework. flutter_rust_bridge's default loader derives the name from the
/// crate (`hfa_ffi.framework/hfa_ffi`), which does not exist.
const appleRustFramework =
    'rust_lib_headphone_for_all.framework/rust_lib_headphone_for_all';

/// How the Rust library is opened on [os] (`Platform.operatingSystem`):
/// `null` = flutter_rust_bridge's default loader (Android, Linux, Windows:
/// `libhfa_ffi.so` / `hfa_ffi.dll`, named after the crate).
///
/// iOS and macOS open [appleRustFramework] and, should the pods ever be
/// linked statically (no framework), fall back to the symbols already linked
/// into the process.
ExternalLibrary? rustExternalLibrary([String? os]) {
  final platform = os ?? (kIsWeb ? 'web' : Platform.operatingSystem);
  if (platform != 'ios' && platform != 'macos') return null;
  try {
    return ExternalLibrary.open(appleRustFramework);
  } catch (e) {
    debugPrint('$appleRustFramework: $e; using the process symbols');
    return ExternalLibrary.process(iKnowHowToUseIt: true);
  }
}

/// Loads the Rust library once (`RustLib.init()`, see [rustExternalLibrary]).
Future<void> loadRustLibrary() async {
  if (_rustLoaded) return;
  await RustLib.init(externalLibrary: rustExternalLibrary());
  _rustLoaded = true;
}

/// The data directory: the native one where the platform provides it
/// (Android `filesDir/hfa`, the iOS App Group container shared with the
/// broadcast extension, macOS Application Support), else
/// `<application support>/hfa` from path_provider.
///
/// With [requireNative] (default: on iOS) a failing or empty `getDataDir` is
/// a startup error instead: the broadcast extension reads trust and
/// settings from the App Group container, so a private fallback directory
/// would make every broadcast fail later without a hint. The fallback is
/// kept where the platform has no channel (desktop: `null`) and on Android,
/// whose path_provider directory is the same `filesDir`.
Future<String> resolveDataDir(
  NativeChannel native, {
  bool? requireNative,
}) async {
  final strict = requireNative ?? (!kIsWeb && Platform.isIOS);
  String? nativeDir;
  try {
    nativeDir = await native.getDataDir();
  } on PlatformException catch (e) {
    if (strict) {
      throw DataDirException(
        'The shared app data folder is not available: '
        '${e.message ?? e.code}',
      );
    }
    debugPrint('getDataDir failed, using path_provider: ${e.message}');
  }
  if (nativeDir != null && nativeDir.isNotEmpty) return nativeDir;
  if (strict) {
    throw const DataDirException(
      'The shared app data folder is not available.',
    );
  }
  final support = await getApplicationSupportDirectory();
  return '${support.path}${Platform.pathSeparator}hfa';
}

/// The data directory could not be resolved (shown on [InitErrorApp]).
class DataDirException implements Exception {
  /// Creates the error with [message].
  const DataDirException(this.message);

  /// What went wrong.
  final String message;

  @override
  String toString() => message;
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
