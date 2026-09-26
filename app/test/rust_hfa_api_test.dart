import 'package:flutter_test/flutter_test.dart';
import 'package:headphone_for_all/src/api/hfa_api.dart';
import 'package:headphone_for_all/src/rust/frb_generated.dart';

/// Records every generated API call made through `RustLib`.
class _MockRustLibApi implements RustLibApi {
  final List<(String, Map<Symbol, Object?>)> calls = [];

  static const info = AppInfo(
    deviceId: 'dev',
    deviceName: 'Mock',
    platform: 'linux',
    version: '9',
    capabilities: CapabilitiesDto(
      systemMix: true,
      perApp: true,
      mutesLocalOutput: false,
      externalOnly: false,
      notes: '',
    ),
  );

  static const settings = SettingsDto(
    deviceName: 'Mock',
    port: 1,
    bitrate: 64000,
    frameMs: 10,
    fec: true,
    jitterMinMs: 10,
    jitterMaxMs: 100,
  );

  static const status = HubStatusDto(
    running: true,
    port: 1,
    deviceName: 'Mock',
    sourceCount: 0,
    advertised: true,
    masterGain: 1,
    addresses: ['192.168.1.2:1'],
  );

  @override
  dynamic noSuchMethod(Invocation invocation) {
    final raw = invocation.memberName.toString();
    final name = raw.substring('Symbol("'.length, raw.length - 2);
    calls.add((name, invocation.namedArguments));
    return switch (name) {
      'crateApiAppInitApp' => Future.value(info),
      'crateApiAppGetSettings' => Future.value(settings),
      'crateApiAppListOutputDevices' => Future.value(<String>['out']),
      'crateApiAppTrustedPeers' => Future.value(<TrustedPeerDto>[]),
      'crateApiAppFingerprintOfKey' => Future.value('ab12-cd34-ef56-7890'),
      'crateApiAppParsePairingUri' => Future.value(
        const PairingUriDto(
          host: 'h',
          port: 1,
          hubId: 'k',
          hubDeviceId: 'd',
          token: 't',
          name: 'n',
        ),
      ),
      'crateApiHubHubStart' || 'crateApiHubHubStatus' => Future.value(status),
      'crateApiHubHubSources' => Future.value(<SourceDto>[]),
      'crateApiHubHubStartPairing' => Future.value(
        const PairingInfoDto(pin: '1', token: 't', uri: 'u', expiresAtUnix: 0),
      ),
      'crateApiHubHubPairingStatus' => Future<PairingInfoDto?>.value(),
      'crateApiHubHubEvents' => Stream<HubEventDto>.value(
        const HubEventDto.error(message: 'e'),
      ),
      'crateApiSenderDiscoverHubs' => Stream<DiscoveryEventDto>.value(
        const DiscoveryEventDto.lost(deviceId: 'x'),
      ),
      'crateApiSenderListCaptureApps' => Future.value(<CaptureAppDto>[]),
      'crateApiSenderSenderStatus' => Future.value(idleSenderStatus),
      'crateApiSenderSenderEvents' => Stream<SenderStatusDto>.value(
        idleSenderStatus,
      ),
      _ => Future<void>.value(),
    };
  }
}

void main() {
  final mock = _MockRustLibApi();
  setUpAll(() => RustLib.initMock(api: mock));
  setUp(mock.calls.clear);

  const api = RustHfaApi();

  Future<(String, Map<Symbol, Object?>)> single(
    Future<Object?> Function() call,
  ) async {
    await call();
    expect(mock.calls, hasLength(1));
    return mock.calls.single;
  }

  test('app functions delegate with the right arguments', () async {
    var (name, args) = await single(
      () => api.initApp(dataDir: '/d', deviceName: 'N'),
    );
    expect(name, 'crateApiAppInitApp');
    expect(args, {#dataDir: '/d', #deviceName: 'N'});

    mock.calls.clear();
    (name, args) = await single(() => api.forgetPeer('peer'));
    expect(name, 'crateApiAppForgetPeer');
    expect(args, {#deviceId: 'peer'});

    mock.calls.clear();
    (name, args) = await single(
      () => api.updateSettings(_MockRustLibApi.settings),
    );
    expect(name, 'crateApiAppUpdateSettings');
    expect(args, {#settings: _MockRustLibApi.settings});

    mock.calls.clear();
    (name, args) = await single(() => api.parsePairingUri('hfa://pair'));
    expect(name, 'crateApiAppParsePairingUri');
    expect(args, {#uri: 'hfa://pair'});

    mock.calls.clear();
    expect(await api.fingerprintOfKey('AQID'), 'ab12-cd34-ef56-7890');
    expect(mock.calls.single.$1, 'crateApiAppFingerprintOfKey');
    expect(mock.calls.single.$2, {#keyB64: 'AQID'});

    for (final (call, expected) in <(Future<Object?> Function(), String)>[
      (api.getSettings, 'crateApiAppGetSettings'),
      (api.listOutputDevices, 'crateApiAppListOutputDevices'),
      (api.trustedPeers, 'crateApiAppTrustedPeers'),
    ]) {
      mock.calls.clear();
      expect((await single(call)).$1, expected);
    }
  });

  test('hub functions delegate with the right arguments', () async {
    final expectations =
        <(Future<Object?> Function(), String, Map<Symbol, Object?>)>[
          (api.hubStart, 'crateApiHubHubStart', {}),
          (api.hubStop, 'crateApiHubHubStop', {}),
          (api.hubStatus, 'crateApiHubHubStatus', {}),
          (api.hubSources, 'crateApiHubHubSources', {}),
          (
            () => api.hubSetGain(3, 0.5),
            'crateApiHubHubSetGain',
            {#streamId: 3, #gain: 0.5},
          ),
          (
            () => api.hubSetMuted(3, true),
            'crateApiHubHubSetMuted',
            {#streamId: 3, #muted: true},
          ),
          (
            () => api.hubSetPriority(4, false),
            'crateApiHubHubSetPriority',
            {#streamId: 4, #priority: false},
          ),
          (
            () => api.hubSetMasterGain(2),
            'crateApiHubHubSetMasterGain',
            {#gain: 2.0},
          ),
          (api.hubStartPairing, 'crateApiHubHubStartPairing', {}),
          (api.hubPairingStatus, 'crateApiHubHubPairingStatus', {}),
          (api.hubCancelPairing, 'crateApiHubHubCancelPairing', {}),
        ];
    for (final (call, name, args) in expectations) {
      mock.calls.clear();
      final (called, calledArgs) = await single(call);
      expect(called, name);
      expect(calledArgs, args);
    }
    mock.calls.clear();
    expect(await api.hubEvents().first, const HubEventDto.error(message: 'e'));
    expect(mock.calls.single.$1, 'crateApiHubHubEvents');
  });

  test('sender functions delegate with the right arguments', () async {
    const request = SenderStartDto(
      hubHost: 'h',
      hubPort: 1,
      source: CaptureSourceDto.system(),
      label: '',
    );
    final expectations =
        <(Future<Object?> Function(), String, Map<Symbol, Object?>)>[
          (api.stopDiscovery, 'crateApiSenderStopDiscovery', {}),
          (api.listCaptureApps, 'crateApiSenderListCaptureApps', {}),
          (
            () => api.senderStart(request),
            'crateApiSenderSenderStart',
            {#request: request},
          ),
          (api.senderStop, 'crateApiSenderSenderStop', {}),
          (api.senderStatus, 'crateApiSenderSenderStatus', {}),
        ];
    for (final (call, name, args) in expectations) {
      mock.calls.clear();
      final (called, calledArgs) = await single(call);
      expect(called, name);
      expect(calledArgs, args);
    }
    mock.calls.clear();
    expect(await api.senderEvents().first, idleSenderStatus);
    expect(
      await api.discoverHubs().first,
      const DiscoveryEventDto.lost(deviceId: 'x'),
    );
    expect(mock.calls.map((c) => c.$1), [
      'crateApiSenderSenderEvents',
      'crateApiSenderDiscoverHubs',
    ]);
  });
}
