/// App preferences the core does not keep (`<data dir>/app_prefs.json`).
library;

import 'dart:async';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/hfa_api.dart';
import '../models/hub_target.dart';
import '../models/source_choice.dart';
import '../util/json_file.dart';
import 'core_providers.dart';
import 'hub_controller.dart';

/// File name of the preferences inside the data directory.
const appPrefsFile = 'app_prefs.json';

/// The hub this device last sent to (offered again as "Send to …").
///
/// Only public data: the hub's device id, name, address and static key (the
/// key is public; it is not proof of a pairing, see [HubTarget.trusted]).
@immutable
class LastHub {
  /// Creates a remembered hub.
  const LastHub({
    required this.name,
    this.deviceId,
    this.host = '',
    this.port = 0,
    this.hubKey,
  });

  /// The hub [target] as it is remembered (the one-time secret is dropped).
  factory LastHub.of(HubTarget target) => LastHub(
    name: target.name,
    deviceId: target.deviceId,
    host: target.host,
    port: target.port,
    hubKey: target.hubKey,
  );

  /// Parses the JSON written by [toJson]; `null` when it does not fit.
  static LastHub? fromJson(Object? json) {
    if (json is! Map) return null;
    final name = json['name'];
    final deviceId = json['deviceId'];
    final host = json['host'];
    final port = json['port'];
    final hubKey = json['hubKey'];
    if (name is! String || name.trim().isEmpty) return null;
    if (deviceId != null && (deviceId is! String || deviceId.isEmpty)) {
      return null;
    }
    if (host is! String || port is! int || port < 0 || port > 65535) {
      return null;
    }
    // Without an id or an address there is nothing to dial.
    if (deviceId == null && host.isEmpty) return null;
    return LastHub(
      name: name,
      deviceId: deviceId as String?,
      host: host,
      port: port,
      hubKey: hubKey is String && hubKey.isNotEmpty ? hubKey : null,
    );
  }

  /// Display name.
  final String name;

  /// Hub device id, when known.
  final String? deviceId;

  /// Where it was reached ('' = found by id).
  final String host;

  /// Port (0 = default).
  final int port;

  /// Hub static key (base64url), from a pairing link.
  final String? hubKey;

  /// The file's JSON.
  Map<String, Object?> toJson() => {
    'name': name,
    'deviceId': deviceId,
    'host': host,
    'port': port,
    'hubKey': hubKey,
  };

  /// The target to select. [trusted] must come from the trust store (this
  /// device paired with the hub as a sender): a remembered hub the user
  /// forgot since then asks for a PIN.
  HubTarget toTarget({required bool trusted}) => HubTarget(
    name: name,
    origin: deviceId == null ? HubOrigin.manual : HubOrigin.paired,
    host: host,
    port: port,
    deviceId: deviceId,
    hubKey: hubKey,
    trusted: trusted,
  );

  @override
  bool operator ==(Object other) =>
      other is LastHub &&
      other.name == name &&
      other.deviceId == deviceId &&
      other.host == host &&
      other.port == port &&
      other.hubKey == hubKey;

  @override
  int get hashCode => Object.hash(name, deviceId, host, port, hubKey);
}

/// The source this device last sent: its kind and, for one app, the app's
/// name (process ids change between runs, so the app is found again by
/// name).
@immutable
class LastSource {
  /// Creates a remembered source.
  const LastSource(this.kind, {this.appName});

  /// The source [choice] as it is remembered.
  factory LastSource.of(SourceChoice choice) =>
      LastSource(choice.kind, appName: choice.app?.name);

  /// Parses the JSON written by [toJson]; `null` when it does not fit.
  static LastSource? fromJson(Object? json) {
    if (json is! Map) return null;
    final kind = SourceKind.values
        .where((k) => k.name == json['kind'])
        .firstOrNull;
    if (kind == null) return null;
    final app = json['appName'];
    return LastSource(
      kind,
      appName: app is String && app.isNotEmpty ? app : null,
    );
  }

  /// What was captured.
  final SourceKind kind;

  /// The app's name, for [SourceKind.app].
  final String? appName;

  /// The file's JSON.
  Map<String, Object?> toJson() => {'kind': kind.name, 'appName': appName};

  /// The choice to preselect: the app among [apps] with the remembered name
  /// (none when it does not run now: the user picks one).
  SourceChoice toChoice([List<CaptureAppDto> apps = const []]) {
    if (kind != SourceKind.app) return SourceChoice(kind);
    return SourceChoice(
      kind,
      app: apps.where((a) => a.name == appName).firstOrNull,
    );
  }

  @override
  bool operator ==(Object other) =>
      other is LastSource && other.kind == kind && other.appName == appName;

  @override
  int get hashCode => Object.hash(kind, appName);
}

/// The app's own preferences.
@immutable
class AppPrefs {
  /// Creates preferences.
  const AppPrefs({
    this.startHubOnLaunch = false,
    this.startAtSignIn = false,
    this.lastHub,
    this.lastSource,
  });

  /// Parses the file's JSON; unknown or wrong fields keep their defaults.
  factory AppPrefs.fromJson(Object? json) {
    if (json is! Map) return const AppPrefs();
    final start = json['startHubOnLaunch'];
    final signIn = json['startAtSignIn'];
    return AppPrefs(
      startHubOnLaunch: start is bool && start,
      startAtSignIn: signIn is bool && signIn,
      lastHub: LastHub.fromJson(json['lastHub']),
      lastSource: LastSource.fromJson(json['lastSource']),
    );
  }

  /// Start the hub when the app opens (e.g. a PC that starts the app at
  /// sign-in, hidden in the tray with `--autostart`, is ready to play).
  final bool startHubOnLaunch;

  /// The user asked to start the app at sign-in (Linux; the switch reads the
  /// autostart entry itself where it can, see `sign_in_launcher.dart`).
  final bool startAtSignIn;

  /// The hub this device last sent to.
  final LastHub? lastHub;

  /// What this device last sent.
  final LastSource? lastSource;

  /// A copy with the given fields replaced.
  AppPrefs copyWith({
    bool? startHubOnLaunch,
    bool? startAtSignIn,
    LastHub? lastHub,
    LastSource? lastSource,
  }) => AppPrefs(
    startHubOnLaunch: startHubOnLaunch ?? this.startHubOnLaunch,
    startAtSignIn: startAtSignIn ?? this.startAtSignIn,
    lastHub: lastHub ?? this.lastHub,
    lastSource: lastSource ?? this.lastSource,
  );

  /// The file's JSON.
  Map<String, Object?> toJson() => {
    'startHubOnLaunch': startHubOnLaunch,
    'startAtSignIn': startAtSignIn,
    if (lastHub != null) 'lastHub': lastHub?.toJson(),
    if (lastSource != null) 'lastSource': lastSource?.toJson(),
  };

  @override
  bool operator ==(Object other) =>
      other is AppPrefs &&
      other.startHubOnLaunch == startHubOnLaunch &&
      other.startAtSignIn == startAtSignIn &&
      other.lastHub == lastHub &&
      other.lastSource == lastSource;

  @override
  int get hashCode =>
      Object.hash(startHubOnLaunch, startAtSignIn, lastHub, lastSource);
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
  bool _loaded = false;

  /// Changes made before the file was loaded, re-applied on top of it.
  final List<AppPrefs Function(AppPrefs)> _early = [];

  File? get _file {
    final dir = ref.read(dataDirProvider);
    return dir == null
        ? null
        : File('$dir${Platform.pathSeparator}$appPrefsFile');
  }

  @override
  AppPrefs build() {
    ref.watch(dataDirProvider);
    _loaded = false;
    _early.clear();
    _loading = _load();
    return const AppPrefs();
  }

  /// Completes once the file was loaded (and the hub asked to start).
  Future<void> get loaded => _loading;

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
    state = _early.fold(loaded, (prefs, change) => change(prefs));
    _early.clear();
    _loaded = true;
    if (state.startHubOnLaunch) {
      // A failure shows like any hub error (app shell snack bar, hub screen).
      await ref.read(hubControllerProvider.notifier).start();
    }
  }

  void _change(AppPrefs Function(AppPrefs) change) {
    if (!_loaded) _early.add(change);
    final next = change(state);
    if (next == state) return;
    state = next;
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

  /// Sets [AppPrefs.startHubOnLaunch].
  void setStartHubOnLaunch(bool value) =>
      _change((p) => p.copyWith(startHubOnLaunch: value));

  /// Records [AppPrefs.startAtSignIn].
  void setStartAtSignIn(bool value) =>
      _change((p) => p.copyWith(startAtSignIn: value));

  /// Remembers where and what this device sent (after a successful start).
  void rememberSend(HubTarget hub, SourceChoice source) => _change(
    (p) =>
        p.copyWith(lastHub: LastHub.of(hub), lastSource: LastSource.of(source)),
  );
}
