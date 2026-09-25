/// The last address each paired hub was reached at.
///
/// iOS cannot browse mDNS (the multicast entitlement is restricted, §8.9), so
/// a paired hub that is not typed in again would have no host, and neither
/// the app's sender nor the broadcast extension could reach it. The app
/// remembers where it last reached each hub (by device id) in
/// `<data dir>/hub_addresses.json` and dials that address on iOS.
library;

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../models/hub_target.dart';
import 'core_providers.dart';

/// File name of the address book inside the data directory.
const hubAddressFile = 'hub_addresses.json';

/// Parses the address book file; entries that do not fit are skipped.
@visibleForTesting
Map<String, HubAddress> parseHubAddresses(String text) {
  final result = <String, HubAddress>{};
  final Object? json;
  try {
    json = jsonDecode(text);
  } on FormatException {
    return result;
  }
  if (json is! Map) return result;
  for (final entry in json.entries) {
    final key = entry.key;
    final value = entry.value;
    if (key is! String || value is! Map) continue;
    final host = value['host'];
    final port = value['port'];
    if (host is! String || host.isEmpty || port is! int) continue;
    if (port < 0 || port > 65535) continue;
    result[key] = HubAddress(host, port);
  }
  return result;
}

/// The last address of each hub, by device id (see the library docs).
final hubAddressBookProvider =
    NotifierProvider<HubAddressBook, Map<String, HubAddress>>(
      HubAddressBook.new,
    );

/// Loads, remembers and saves hub addresses. Without a data directory
/// ([dataDirProvider] is `null`: tests, demo mode) it only keeps them in
/// memory. File errors are logged, never thrown: the book is a convenience.
class HubAddressBook extends Notifier<Map<String, HubAddress>> {
  Future<void> _loading = Future.value();
  Future<void> _saving = Future.value();

  /// Completes once the file was loaded and every change so far written.
  @visibleForTesting
  Future<void> flush() async {
    await _loading;
    await _saving;
  }

  File? get _file {
    final dir = ref.read(dataDirProvider);
    return dir == null
        ? null
        : File('$dir${Platform.pathSeparator}$hubAddressFile');
  }

  @override
  Map<String, HubAddress> build() {
    ref.watch(dataDirProvider);
    _loading = _load();
    return const {};
  }

  Future<void> _load() async {
    final file = _file;
    if (file == null) return;
    try {
      if (!await file.exists()) return;
      final loaded = parseHubAddresses(await file.readAsString());
      if (!ref.mounted) return;
      // Addresses remembered meanwhile win.
      state = {...loaded, ...state};
    } catch (e) {
      debugPrint('hub addresses: $e');
    }
  }

  /// Remembers that the hub [deviceId] was reached at [host]:[port]. An
  /// empty host (found by id) is ignored.
  void remember(String deviceId, String host, int port) {
    if (deviceId.isEmpty || host.isEmpty) return;
    final address = HubAddress(host, port);
    if (state[deviceId] == address) return;
    state = {...state, deviceId: address};
    _save();
  }

  /// Forgets the address of [deviceId] (e.g. the hub was forgotten).
  void forget(String deviceId) {
    if (!state.containsKey(deviceId)) return;
    state = {...state}..remove(deviceId);
    _save();
  }

  void _save() {
    final file = _file;
    if (file == null) return;
    final json = jsonEncode({
      for (final e in state.entries)
        e.key: {'host': e.value.host, 'port': e.value.port},
    });
    // One write at a time, in order; each writes a temp file and renames it.
    _saving = _saving.then((_) async {
      try {
        final tmp = File('${file.path}.tmp');
        await tmp.writeAsString(json, flush: true);
        await tmp.rename(file.path);
      } catch (e) {
        debugPrint('hub addresses: $e');
      }
    });
  }
}
