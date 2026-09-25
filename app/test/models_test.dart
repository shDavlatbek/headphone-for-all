import 'package:flutter_rust_bridge/flutter_rust_bridge.dart'
    show AnyhowException, PanicException;
import 'package:flutter_test/flutter_test.dart';
import 'package:headphone_for_all/src/api/hfa_api.dart';
import 'package:headphone_for_all/src/models/hub_target.dart';
import 'package:headphone_for_all/src/models/source_choice.dart';
import 'package:headphone_for_all/src/util/format.dart';
import 'package:headphone_for_all/src/widgets/level_meter.dart';

AppInfo infoFor(String platform, {CapabilitiesDto? caps}) => AppInfo(
  deviceId: 'id',
  deviceName: 'n',
  platform: platform,
  version: '1',
  capabilities: caps ?? FakeHfaApi.capabilitiesFor(platform),
);

void main() {
  group('availableSources', () {
    test('desktop with per-app capture offers everything', () {
      expect(availableSources(infoFor('windows')), [
        SourceKind.system,
        SourceKind.systemExceptThisApp,
        SourceKind.app,
        SourceKind.tone,
      ]);
    });

    test('Android offers native device audio and the tone', () {
      expect(availableSources(infoFor('android')), [
        SourceKind.deviceAudio,
        SourceKind.tone,
      ]);
    });

    test('iOS offers the broadcast and the tone', () {
      expect(availableSources(infoFor('ios')), [
        SourceKind.broadcast,
        SourceKind.tone,
      ]);
    });

    test('system mix without per-app hides "One app"', () {
      const caps = CapabilitiesDto(
        systemMix: true,
        perApp: false,
        mutesLocalOutput: false,
        externalOnly: false,
        notes: '',
      );
      expect(
        availableSources(infoFor('linux', caps: caps)),
        isNot(contains(SourceKind.app)),
      );
    });

    test('no capture at all leaves the tone', () {
      const caps = CapabilitiesDto(
        systemMix: false,
        perApp: false,
        mutesLocalOutput: false,
        externalOnly: false,
        notes: '',
      );
      expect(availableSources(infoFor('unknown', caps: caps)), [
        SourceKind.tone,
      ]);
    });
  });

  group('SourceChoice', () {
    test('maps to the core capture sources', () {
      expect(
        const SourceChoice(SourceKind.system).toDto(),
        const CaptureSourceDto.system(),
      );
      expect(
        const SourceChoice(SourceKind.systemExceptThisApp).toDto(),
        const CaptureSourceDto.systemExcludingSelf(),
      );
      const app = CaptureAppDto(pid: 42, name: 'Player');
      const choice = SourceChoice(SourceKind.app, app: app);
      expect(choice.toDto(), const CaptureSourceDto.process(pid: 42));
      expect(choice.label, 'Player');
      expect(
        const SourceChoice(SourceKind.deviceAudio).toDto(),
        const CaptureSourceDto.external_(
          feedId: androidFeedId,
          sampleRate: 48000,
          channels: 2,
        ),
      );
      expect(
        const SourceChoice(SourceKind.tone).toDto(),
        const CaptureSourceDto.tone(freqHz: 440),
      );
    });

    test('an app choice without an app is incomplete', () {
      expect(const SourceChoice(SourceKind.app).isComplete, isFalse);
      expect(const SourceChoice(SourceKind.system).isComplete, isTrue);
    });
  });

  group('HubTarget', () {
    const hub = HubInfoDto(
      deviceId: 'abcd',
      name: 'Desk',
      addrs: ['10.0.0.2', 'fe80::2'],
      port: 47810,
      platform: 'linux',
      trusted: false,
    );

    test(
      'discovered hubs dial the first address and need a PIN if untrusted',
      () {
        final t = HubTarget.discovered(hub);
        expect(t.host, '10.0.0.2');
        expect(t.needsPin, isTrue);
        expect(t.withPin('123456').needsPin, isFalse);
        final request = t
            .withPin(' 123456 ')
            .toRequest(const CaptureSourceDto.system(), label: 'x');
        expect(request.hubHost, '10.0.0.2');
        expect(request.hubPort, 47810);
        expect(request.hubDeviceId, 'abcd');
        expect(request.hubKey, isNull);
        expect(request.pairingSecret, '123456');
        expect(request.label, 'x');
      },
    );

    test('paired peers are found by id (empty host)', () {
      final t = HubTarget.paired(
        const TrustedPeerDto(deviceId: 'ef01', name: 'Old', pairedAtUnix: 0),
      );
      expect(t.host, isEmpty);
      expect(t.needsPin, isFalse);
      expect(t.toRequest(const CaptureSourceDto.system()).hubDeviceId, 'ef01');
    });

    test('pairing links carry key and token', () {
      final t = HubTarget.fromPairingUri(
        const PairingUriDto(
          host: 'fe80::5',
          port: 47811,
          hubId: 'KEY',
          hubDeviceId: 'dev',
          token: 'TOKEN',
          name: 'Hub',
        ),
      );
      final r = t.toRequest(const CaptureSourceDto.system());
      expect(r.hubKey, 'KEY');
      expect(r.pairingSecret, 'TOKEN');
      expect(t.address, '[fe80::5]:47811');
      expect(t.needsPin, isFalse);
    });

    test('manual targets keep an optional PIN', () {
      expect(HubTarget.manual(host: 'h').pairingSecret, isNull);
      expect(HubTarget.manual(host: 'h', pin: ' ').pairingSecret, isNull);
      expect(
        HubTarget.manual(host: 'h', pin: '000111').pairingSecret,
        '000111',
      );
      expect(HubTarget.manual(host: 'h', port: 9).address, 'h:9');
    });

    test('isValidPin', () {
      expect(isValidPin('123456'), isTrue);
      expect(isValidPin(' 123456 '), isTrue);
      expect(isValidPin('12345'), isFalse);
      expect(isValidPin('12345a'), isFalse);
    });
  });

  group('format', () {
    test('levels, gains and times', () {
      expect(formatDb(-120), 'silent');
      expect(formatDb(-17.6), '-18 dB');
      expect(formatGain(1.25), '125%');
      expect(formatBitrate(128000), '128 kbit/s');
      expect(formatBitrate(0), '—');
      expect(formatCountdown(299), '4:59');
      expect(formatCountdown(-5), '0:00');
      expect(formatPin('482913'), '482 913');
      expect(formatPercent(0.44), '0.4%');
      expect(senderStateLabel('streaming'), 'Streaming');
    });

    test('meter fraction', () {
      expect(LevelMeter.fraction(-120), 0);
      expect(LevelMeter.fraction(-30), closeTo(0.5, 1e-9));
      expect(LevelMeter.fraction(3), 1);
      expect(LevelMeter.fraction(double.nan), 0);
    });
  });

  test('describeError extracts the core message', () {
    expect(describeError(const HfaApiException('boom')), 'boom');
    expect(describeError(StateError('bad')), 'bad');
    expect(
      describeError(AnyhowException('the hub is not running\n\nStack:\n0: x')),
      'the hub is not running',
    );
    expect(
      describeError(PanicException('todo: engineBacktrace [{ fn: "x" }]')),
      'Internal error: todo: engine',
    );
  });
}
