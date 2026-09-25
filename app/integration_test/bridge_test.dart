// Runs against the real Rust library (built by cargokit):
//   flutter test integration_test -d linux   (or a device id)

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:headphone_for_all/src/bootstrap.dart';
import 'package:headphone_for_all/src/rust/api/app.dart';
import 'package:headphone_for_all/src/rust/api/hub.dart';
import 'package:headphone_for_all/src/rust/api/sender.dart';
import 'package:integration_test/integration_test.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  // The app's own loader (it names the Apple framework explicitly).
  setUpAll(loadRustLibrary);

  test('parses a pairing URI', () async {
    const key = 'AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE';
    final dto = await parsePairingUri(
      uri: 'hfa://pair?v=0&h=192.168.1.5&p=47810&id=$key&t=dG9rZW4&n=Desk',
    );
    expect(dto.host, '192.168.1.5');
    expect(dto.port, 47810);
    expect(dto.hubId, key);
    expect(dto.hubDeviceId, matches(RegExp(r'^[0-9a-f]{4}(-[0-9a-f]{4}){3}$')));
    expect(dto.token, 'dG9rZW4');
    expect(dto.name, 'Desk');
  });

  test('rejects a malformed pairing URI', () async {
    await expectLater(
      parsePairingUri(uri: 'https://example.com'),
      throwsA(anything),
    );
  });

  test('engines report stopped before initialization', () async {
    final hub = await hubStatus();
    expect(hub.running, isFalse);
    expect(hub.port, 0);
    expect((await senderStatus()).state, 'idle');
    await expectLater(getSettings(), throwsA(anything));
  });

  test('sender status stream starts with the current status', () async {
    final first = await senderEvents().first;
    expect(first.state, 'idle');
  });

  // Everything below runs after initApp (the tests of a file run in order).
  late Directory dataDir;
  late AppInfo info;

  test('initApp creates the identity and settings (idempotent)', () async {
    // Kept for the later tests; removed in tearDownAll.
    dataDir = await Directory.systemTemp.createTemp('hfa_bridge_');
    info = await initApp(dataDir: dataDir.path, deviceName: 'Bridge test');
    expect(info.deviceName, 'Bridge test');
    expect(info.deviceId, matches(RegExp(r'^[0-9a-f]{4}(-[0-9a-f]{4}){3}$')));
    expect(info.version, isNotEmpty);
    expect(
      File('${dataDir.path}${Platform.pathSeparator}settings.json')
          .existsSync(),
      isTrue,
    );
    // Same directory again: same identity, the first-run name is not reused.
    final again = await initApp(dataDir: dataDir.path, deviceName: 'Other');
    expect(again.deviceId, info.deviceId);
    expect(again.deviceName, 'Bridge test');
  });

  test('settings round-trip through the Dart codecs', () async {
    final before = await getSettings();
    await updateSettings(
      settings: SettingsDto(
        deviceName: 'Renamed',
        port: 0,
        bitrate: 96000,
        frameMs: 10,
        fec: !before.fec,
        jitterMinMs: before.jitterMinMs,
        jitterMaxMs: before.jitterMaxMs,
        outputDevice: null,
      ),
    );
    final after = await getSettings();
    expect(after.deviceName, 'Renamed');
    expect(after.bitrate, 96000);
    expect(after.frameMs, 10);
    expect(after.fec, !before.fec);
    await expectLater(
      updateSettings(
        settings: SettingsDto(
          deviceName: '',
          port: 0,
          bitrate: 96000,
          frameMs: 10,
          fec: true,
          jitterMinMs: after.jitterMinMs,
          jitterMaxMs: after.jitterMaxMs,
        ),
      ),
      throwsA(anything),
    );
    expect(await trustedPeers(), isEmpty);
  });

  test('the hub starts, pairs and restarts (when an output exists)', () async {
    final HubStatusDto status;
    try {
      status = await hubStart();
    } catch (e) {
      // CI machines may have no audio output device at all.
      // ignore: avoid_print
      print('hub start skipped: $e');
      return;
    }
    try {
      expect(status.running, isTrue);
      expect(status.port, greaterThan(0));
      expect((await hubStatus()).running, isTrue);
      expect(await hubSources(), isEmpty);

      final pairing = await hubStartPairing();
      expect(pairing.pin, matches(RegExp(r'^\d{6}$')));
      final uri = await parsePairingUri(uri: pairing.uri);
      expect(uri.port, status.port);
      expect(uri.hubDeviceId, info.deviceId);
      await hubCancelPairing();

      // One subscription for the app's lifetime: it survives a restart.
      final events = hubEvents();
      final sub = events.listen((_) {});
      await hubStop();
      expect((await hubStatus()).running, isFalse);
      final restarted = await hubStart();
      expect(restarted.running, isTrue);
      await sub.cancel();
    } finally {
      await hubStop();
    }
  });

  tearDownAll(() async {
    try {
      await dataDir.delete(recursive: true);
    } catch (_) {}
  });
}
