// Runs against the real Rust library (built by cargokit):
//   flutter test integration_test -d linux   (or a device id)

import 'package:flutter_test/flutter_test.dart';
import 'package:headphone_for_all/src/rust/api/app.dart';
import 'package:headphone_for_all/src/rust/api/hub.dart';
import 'package:headphone_for_all/src/rust/api/sender.dart';
import 'package:headphone_for_all/src/rust/frb_generated.dart';
import 'package:integration_test/integration_test.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  setUpAll(() async => RustLib.init());

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
}
