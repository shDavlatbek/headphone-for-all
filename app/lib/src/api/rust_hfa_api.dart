import '../rust/api/app.dart' as app;
import '../rust/api/hub.dart' as hub;
import '../rust/api/sender.dart' as sender;
import 'hfa_api.dart';

/// [HfaApi] backed by the real Rust core through the generated
/// flutter_rust_bridge functions. `RustLib.init()` must have completed first.
class RustHfaApi implements HfaApi {
  /// Creates the adapter (stateless: the core holds all state).
  const RustHfaApi();

  @override
  Future<AppInfo> initApp({required String dataDir, String? deviceName}) =>
      app.initApp(dataDir: dataDir, deviceName: deviceName);

  @override
  Future<SettingsDto> getSettings() => app.getSettings();

  @override
  Future<void> updateSettings(SettingsDto settings) =>
      app.updateSettings(settings: settings);

  @override
  Future<List<String>> listOutputDevices() => app.listOutputDevices();

  @override
  Future<List<TrustedPeerDto>> trustedPeers() => app.trustedPeers();

  @override
  Future<void> forgetPeer(String deviceId) =>
      app.forgetPeer(deviceId: deviceId);

  @override
  Future<PairingUriDto> parsePairingUri(String uri) =>
      app.parsePairingUri(uri: uri);

  @override
  Future<HubStatusDto> hubStart() => hub.hubStart();

  @override
  Future<void> hubStop() => hub.hubStop();

  @override
  Future<HubStatusDto> hubStatus() => hub.hubStatus();

  @override
  Future<List<SourceDto>> hubSources() => hub.hubSources();

  @override
  Future<void> hubSetGain(int streamId, double gain) =>
      hub.hubSetGain(streamId: streamId, gain: gain);

  @override
  Future<void> hubSetMuted(int streamId, bool muted) =>
      hub.hubSetMuted(streamId: streamId, muted: muted);

  @override
  Future<void> hubSetPriority(int streamId, bool priority) =>
      hub.hubSetPriority(streamId: streamId, priority: priority);

  @override
  Future<void> hubSetMasterGain(double gain) =>
      hub.hubSetMasterGain(gain: gain);

  @override
  Future<PairingInfoDto> hubStartPairing() => hub.hubStartPairing();

  @override
  Future<PairingInfoDto?> hubPairingStatus() => hub.hubPairingStatus();

  @override
  Future<void> hubCancelPairing() => hub.hubCancelPairing();

  @override
  Stream<HubEventDto> hubEvents() => hub.hubEvents();

  @override
  Stream<DiscoveryEventDto> discoverHubs() => sender.discoverHubs();

  @override
  Future<void> stopDiscovery() => sender.stopDiscovery();

  @override
  Future<List<CaptureAppDto>> listCaptureApps() => sender.listCaptureApps();

  @override
  Future<void> senderStart(SenderStartDto request) =>
      sender.senderStart(request: request);

  @override
  Future<void> senderStop() => sender.senderStop();

  @override
  Future<SenderStatusDto> senderStatus() => sender.senderStatus();

  @override
  Stream<SenderStatusDto> senderEvents() => sender.senderEvents();
}
