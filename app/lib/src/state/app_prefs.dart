/// App preferences the core does not keep (`<data dir>/app_prefs.json`).
library;

import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/hfa_api.dart';
import '../util/json_file.dart';
import 'core_providers.dart';
import 'hub_controller.dart';

/// File name of the preferences inside the data directory.
const appPrefsFile = 'app_prefs.json';

/// The app's own preferences.
@immutable
class AppPrefs {
  /// Creates preferences.
  const AppPrefs({this.startHubOnLaunch = false});

  /// Parses the file's JSON; unknown or wrong fields keep their defaults.
  factory AppPrefs.fromJson(Object? json) {
    if (json is! Map) return const AppPrefs();
    final start = json['startHubOnLaunch'];
    return AppPrefs(startHubOnLaunch: start is bool && start);
  }

  /// Start the hub when the app opens (e.g. a PC that starts the app at
  /// sign-in, hidden in the tray with `--autostart`, is ready to play).
  final bool startHubOnLaunch;

  /// The file's JSON.
  Map<String, Object?> toJson() => {'startHubOnLaunch': startHubOnLaunch};
}

/// The app preferences; building it (the app shell does, at launch) also
/// starts the hub when [AppPrefs.startHubOnLaunch] is set.
final appPrefsProvider = NotifierProvider<AppPrefsNotifier, AppPrefs>(
  AppPrefsNotifier.new,
);

/// Loads and saves [AppPrefs]. Without a data directory (tests, demo mode)
/// they only live in memory. File errors are logged, never thrown.
class AppPrefsNotifier extends Notifier<AppPrefs> {
  Future<void> _loading = Future.value();
  Future<void> _saving = Future.value();
  bool _changed = false;

  File? get _file {
    final dir = ref.read(dataDirProvider);
    return dir == null
        ? null
        : File('$dir${Platform.pathSeparator}$appPrefsFile');
  }

  @override
  AppPrefs build() {
    ref.watch(dataDirProvider);
    _loading = _load();
    return const AppPrefs();
  }

  /// Completes once the file was loaded (and the hub asked to start) and
  /// every change so far written.
  @visibleForTesting
  Future<void> flush() async {
    await _loading;
    await _saving;
  }

  Future<void> _load() async {
    final file = _file;
    AppPrefs loaded = const AppPrefs();
    if (file != null) {
      try {
        loaded = AppPrefs.fromJson(await readJsonFile(file));
      } catch (e) {
        debugPrint('app prefs: $e');
      }
    }
    if (!ref.mounted) return;
    if (!_changed) state = loaded;
    if (state.startHubOnLaunch) {
      // A failure shows like any hub error (app shell snack bar, hub screen).
      await ref.read(hubControllerProvider.notifier).start();
    }
  }

  /// Sets [AppPrefs.startHubOnLaunch].
  void setStartHubOnLaunch(bool value) {
    _changed = true;
    state = AppPrefs(startHubOnLaunch: value);
    final file = _file;
    if (file == null) return;
    final json = state.toJson();
    _saving = _saving.then((_) async {
      try {
        await writeJsonFile(file, json);
      } catch (e) {
        debugPrint('app prefs: ${describeError(e)}');
      }
    });
  }
}
