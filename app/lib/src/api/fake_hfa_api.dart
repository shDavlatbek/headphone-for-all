import 'dart:async';
import 'dart:math' as math;

import 'hfa_api.dart';

/// An in-memory [HfaApi] for widget tests and the demo mode
/// (`flutter run --dart-define=HFA_FAKE=true`).
///
/// It mimics the core's documented behaviour (idempotent start/stop, one live
/// sender at a time, "pairing required" for an unpaired hub, settings
/// validation) without any audio or networking. Tests drive it through the
/// `emit*` / `add*` helpers and inspect [calls], [lastSenderStart] and the
/// stored [sources]. [FakeHfaApi.demo] additionally animates levels and
/// discovers a few hubs, using timers (never use it in widget tests).
class FakeHfaApi implements HfaApi {
  /// A deterministic fake (no timers). [platform] is the value reported in
  /// [AppInfo.platform]; [capabilities] default to what that platform can do.
  FakeHfaApi({
    String platform = 'linux',
    CapabilitiesDto? capabilities,
    String deviceName = 'Test device',
    List<HubInfoDto>? discoverableHubs,
    List<TrustedPeerDto>? trusted,
    List<CaptureAppDto>? captureApps,
    List<String>? outputDevices,
    this.initError,
    this.connectAutomatically = true,
  }) : appInfo = AppInfo(
         deviceId: 'fa4e-0000-0000-0001',
         deviceName: deviceName,
         platform: platform,
         version: '0.1.0',
         capabilities: capabilities ?? capabilitiesFor(platform),
       ),
       discoverableHubs = discoverableHubs ?? [],
       trusted = trusted ?? [],
       captureApps = captureApps ?? [],
       outputDevices = outputDevices ?? ['Speakers', 'USB Headphones'],
       settings = SettingsDto(
         deviceName: deviceName,
         port: 47810,
         bitrate: 128000,
         frameMs: 10,
         fec: true,
         jitterMinMs: 20,
         jitterMaxMs: 200,
       ),
       _demo = false;

  /// A lively fake for the demo mode: two sources on the hub, three hubs on
  /// the LAN, animated meters. Call [dispose] to stop its timers.
  FakeHfaApi.demo({String platform = 'linux'})
    : appInfo = AppInfo(
        deviceId: 'de30-0000-0000-0001',
        deviceName: 'Demo device',
        platform: platform,
        version: '0.1.0',
        capabilities: capabilitiesFor(platform),
      ),
      discoverableHubs = [
        const HubInfoDto(
          deviceId: 'a1b2-c3d4-e5f6-0001',
          name: 'Living-room PC',
          addrs: ['192.168.1.20'],
          port: 47810,
          platform: 'windows',
          trusted: true,
        ),
        const HubInfoDto(
          deviceId: 'a1b2-c3d4-e5f6-0002',
          name: 'Work MacBook',
          addrs: ['192.168.1.31'],
          port: 47810,
          platform: 'macos',
          trusted: false,
        ),
      ],
      trusted = [
        const TrustedPeerDto(
          deviceId: 'a1b2-c3d4-e5f6-0001',
          name: 'Living-room PC',
          pairedAtUnix: 1767225600,
        ),
        const TrustedPeerDto(
          deviceId: 'a1b2-c3d4-e5f6-0009',
          name: 'Old tablet',
          pairedAtUnix: 1735689600,
        ),
      ],
      captureApps = const [
        CaptureAppDto(pid: 4242, name: 'Firefox'),
        CaptureAppDto(pid: 5151, name: 'Spotify'),
        CaptureAppDto(pid: 6161, name: 'Zoom'),
      ],
      outputDevices = ['Speakers', 'USB Headphones', 'Bluetooth Headset'],
      settings = const SettingsDto(
        deviceName: 'Demo device',
        port: 47810,
        bitrate: 128000,
        frameMs: 10,
        fec: true,
        jitterMinMs: 20,
        jitterMaxMs: 200,
      ),
      initError = null,
      connectAutomatically = true,
      _demo = true {
    _seedDemoSources();
  }

  /// Capabilities the real core reports on [platform].
  static CapabilitiesDto capabilitiesFor(String platform) {
    return switch (platform) {
      'android' => const CapabilitiesDto(
        systemMix: false,
        perApp: false,
        mutesLocalOutput: false,
        externalOnly: true,
        notes:
            'Android captures playback through MediaProjection (Android 10+). '
            'Apps that opt out of capture stay silent.',
      ),
      'ios' => const CapabilitiesDto(
        systemMix: false,
        perApp: false,
        mutesLocalOutput: false,
        externalOnly: true,
        notes:
            'iOS shares audio through a screen broadcast. Protected (DRM) '
            'content is silent.',
      ),
      'macos' => const CapabilitiesDto(
        systemMix: true,
        perApp: true,
        mutesLocalOutput: true,
        externalOnly: false,
        notes: 'Core Audio process taps (macOS 14.4+).',
      ),
      'windows' => const CapabilitiesDto(
        systemMix: true,
        perApp: true,
        mutesLocalOutput: false,
        externalOnly: false,
        notes: 'WASAPI loopback; per-app capture needs Windows 10 2004+.',
      ),
      _ => const CapabilitiesDto(
        systemMix: true,
        perApp: true,
        mutesLocalOutput: false,
        externalOnly: false,
        notes: 'PipeWire capture.',
      ),
    };
  }

  /// What [initApp] returns.
  AppInfo appInfo;

  /// Thrown by [initApp] when set.
  Object? initError;

  /// Whether a started sender reaches `streaming` by itself.
  bool connectAutomatically;

  /// Hubs announced to every [discoverHubs] listener.
  final List<HubInfoDto> discoverableHubs;

  /// The trust store.
  final List<TrustedPeerDto> trusted;

  /// What [listCaptureApps] returns.
  final List<CaptureAppDto> captureApps;

  /// What [listOutputDevices] returns.
  final List<String> outputDevices;

  /// The saved settings.
  SettingsDto settings;

  /// Every method called, by name, in order.
  final List<String> calls = [];

  /// The request of the last [senderStart] call.
  SenderStartDto? lastSenderStart;

  /// The last master gain set.
  double masterGain = 1;

  /// The open pairing window, if any.
  PairingInfoDto? pairing;

  final bool _demo;
  final Map<int, SourceDto> _sources = {};
  final StreamController<HubEventDto> _hubEvents = StreamController.broadcast();
  final StreamController<SenderStatusDto> _senderEvents =
      StreamController.broadcast();
  StreamController<DiscoveryEventDto>? _discovery;
  bool _hubRunning = false;
  SenderStatusDto _senderStatus = idleStatus;
  final List<Timer> _timers = [];
  int _nextStreamId = 100;
  int _tick = 0;
  Timer? _demoTicker;

  /// The status of a sender that is not running.
  static const idleStatus = idleSenderStatus;

  /// Current hub sources, by stream id.
  Map<int, SourceDto> get sources => Map.unmodifiable(_sources);

  /// Whether the hub runs.
  bool get hubRunning => _hubRunning;

  /// The current sender status.
  SenderStatusDto get senderStatusNow => _senderStatus;

  /// Whether a discovery stream is open.
  bool get discovering => _discovery != null;

  /// Stops the demo timers and closes the streams.
  void dispose() {
    for (final timer in _timers) {
      timer.cancel();
    }
    _timers.clear();
    _discovery?.close();
    _hubEvents.close();
    _senderEvents.close();
  }

  // ---------------------------------------------------------------- helpers

  /// Adds (or replaces) a hub source and, unless [emit] is false, emits
  /// `SourceAdded` (false simulates a missed event).
  void addSource(SourceDto source, {bool emit = true}) {
    _sources[source.streamId] = source;
    if (_hubRunning && emit) {
      _hubEvents.add(HubEventDto.sourceAdded(source));
    }
  }

  /// Removes a hub source and emits `SourceRemoved`.
  void removeSource(int streamId) {
    _sources.remove(streamId);
    if (_hubRunning) {
      _hubEvents.add(HubEventDto.sourceRemoved(streamId: streamId));
    }
  }

  /// Emits any hub event.
  void emitHubEvent(HubEventDto event) => _hubEvents.add(event);

  /// Emits a discovery event on the open discovery stream.
  void emitDiscovery(DiscoveryEventDto event) => _discovery?.add(event);

  /// Sets and emits a sender status.
  void emitSenderStatus(SenderStatusDto status) {
    _senderStatus = status;
    _senderEvents.add(status);
  }

  /// Simulates a sender pairing through the open pairing window.
  void completePairing({required String deviceId, required String name}) {
    pairing = null;
    trusted.add(
      TrustedPeerDto(
        deviceId: deviceId,
        name: name,
        pairedAtUnix: DateTime.now().millisecondsSinceEpoch ~/ 1000,
      ),
    );
    _hubEvents.add(
      HubEventDto.pairingCompleted(deviceId: deviceId, name: name),
    );
  }

  /// A source with sensible defaults, for tests.
  static SourceDto source({
    required int streamId,
    String deviceName = 'Laptop',
    String label = 'System audio',
    String platform = 'windows',
    double gain = 1,
    bool muted = false,
    bool priority = false,
    bool active = true,
    double levelDb = -18,
  }) {
    return SourceDto(
      streamId: streamId,
      deviceId: 'dev-$streamId',
      deviceName: deviceName,
      label: label,
      platform: platform,
      gain: gain,
      muted: muted,
      priority: priority,
      active: active,
      lossPct: 0.4,
      jitterMs: 3.2,
      bufferMs: 40,
      latencyMs: 55,
      levelDb: levelDb,
    );
  }

  SourceDto _copySource(
    SourceDto s, {
    double? gain,
    bool? muted,
    bool? priority,
    double? levelDb,
  }) {
    return SourceDto(
      streamId: s.streamId,
      deviceId: s.deviceId,
      deviceName: s.deviceName,
      label: s.label,
      platform: s.platform,
      gain: gain ?? s.gain,
      muted: muted ?? s.muted,
      priority: priority ?? s.priority,
      active: s.active,
      lossPct: s.lossPct,
      jitterMs: s.jitterMs,
      bufferMs: s.bufferMs,
      latencyMs: s.latencyMs,
      levelDb: levelDb ?? s.levelDb,
    );
  }

  void _updateSource(int streamId, SourceDto Function(SourceDto) change) {
    final current = _sources[streamId];
    if (current == null) {
      throw HfaApiException('unknown stream $streamId');
    }
    final next = change(current);
    _sources[streamId] = next;
    _hubEvents.add(HubEventDto.sourceUpdated(next));
  }

  void _requireHub() {
    if (!_hubRunning) throw const HfaApiException('the hub is not running');
  }

  static void _checkGain(double gain) {
    if (!gain.isFinite || gain < 0 || gain > 4) {
      throw HfaApiException('gain must be within 0..=4, got $gain');
    }
  }

  // -------------------------------------------------------------------- app

  @override
  Future<AppInfo> initApp({required String dataDir, String? deviceName}) async {
    calls.add('initApp');
    final error = initError;
    if (error != null) throw error;
    return appInfo;
  }

  @override
  Future<SettingsDto> getSettings() async {
    calls.add('getSettings');
    return settings;
  }

  @override
  Future<void> updateSettings(SettingsDto settings) async {
    calls.add('updateSettings');
    final name = settings.deviceName.trim();
    if (name.isEmpty || name.length > 64) {
      throw const HfaApiException(
        'invalid device_name: must be 1..=64 characters',
      );
    }
    if (settings.bitrate < 6000 || settings.bitrate > 510000) {
      throw const HfaApiException('invalid bitrate: 6000..=510000');
    }
    if (settings.frameMs != 10 && settings.frameMs != 20) {
      throw const HfaApiException('invalid frame_ms: 10 or 20');
    }
    if (settings.jitterMinMs < 1 ||
        settings.jitterMinMs > settings.jitterMaxMs ||
        settings.jitterMaxMs > 2000) {
      throw const HfaApiException('invalid jitter range');
    }
    this.settings = SettingsDto(
      deviceName: name,
      port: settings.port,
      bitrate: settings.bitrate,
      frameMs: settings.frameMs,
      fec: settings.fec,
      jitterMinMs: settings.jitterMinMs,
      jitterMaxMs: settings.jitterMaxMs,
      outputDevice: settings.outputDevice,
    );
    appInfo = AppInfo(
      deviceId: appInfo.deviceId,
      deviceName: name,
      platform: appInfo.platform,
      version: appInfo.version,
      capabilities: appInfo.capabilities,
    );
  }

  @override
  Future<List<String>> listOutputDevices() async {
    calls.add('listOutputDevices');
    return List.of(outputDevices);
  }

  @override
  Future<List<TrustedPeerDto>> trustedPeers() async {
    calls.add('trustedPeers');
    return List.of(trusted);
  }

  @override
  Future<void> forgetPeer(String deviceId) async {
    calls.add('forgetPeer');
    trusted.removeWhere((p) => p.deviceId == deviceId);
  }

  @override
  Future<PairingUriDto> parsePairingUri(String uri) async {
    calls.add('parsePairingUri');
    final parsed = Uri.tryParse(uri.trim());
    if (parsed == null || parsed.scheme != 'hfa' || parsed.host != 'pair') {
      throw const HfaApiException('not a pairing URI');
    }
    final q = parsed.queryParameters;
    final host = q['h'];
    final id = q['id'];
    final token = q['t'];
    if (host == null || id == null || token == null) {
      throw const HfaApiException('pairing URI is missing a field');
    }
    return PairingUriDto(
      host: host,
      port: int.tryParse(q['p'] ?? '') ?? 47810,
      hubId: id,
      hubDeviceId: 'f00d-${id.hashCode.toRadixString(16).padLeft(4, '0')}',
      token: token,
      name: q['n'] ?? 'Hub',
    );
  }

  // -------------------------------------------------------------------- hub

  HubStatusDto _hubStatus() => HubStatusDto(
    running: _hubRunning,
    port: _hubRunning ? settings.port : 0,
    deviceName: appInfo.deviceName,
    sourceCount: _hubRunning ? _sources.length : 0,
  );

  @override
  Future<HubStatusDto> hubStart() async {
    calls.add('hubStart');
    if (!_hubRunning) {
      _hubRunning = true;
      for (final s in _sources.values) {
        _hubEvents.add(HubEventDto.sourceAdded(s));
      }
      if (_demo) _startDemoTimer();
    }
    return _hubStatus();
  }

  @override
  Future<void> hubStop() async {
    calls.add('hubStop');
    _hubRunning = false;
    pairing = null;
  }

  @override
  Future<HubStatusDto> hubStatus() async => _hubStatus();

  @override
  Future<List<SourceDto>> hubSources() async {
    return _hubRunning ? _sources.values.toList() : const [];
  }

  @override
  Future<void> hubSetGain(int streamId, double gain) async {
    calls.add('hubSetGain');
    _requireHub();
    _checkGain(gain);
    _updateSource(streamId, (s) => _copySource(s, gain: gain));
  }

  @override
  Future<void> hubSetMuted(int streamId, bool muted) async {
    calls.add('hubSetMuted');
    _requireHub();
    _updateSource(streamId, (s) => _copySource(s, muted: muted));
  }

  @override
  Future<void> hubSetPriority(int streamId, bool priority) async {
    calls.add('hubSetPriority');
    _requireHub();
    _updateSource(streamId, (s) => _copySource(s, priority: priority));
  }

  @override
  Future<void> hubSetMasterGain(double gain) async {
    calls.add('hubSetMasterGain');
    _requireHub();
    _checkGain(gain);
    masterGain = gain;
  }

  @override
  Future<PairingInfoDto> hubStartPairing() async {
    calls.add('hubStartPairing');
    _requireHub();
    final expires = DateTime.now().millisecondsSinceEpoch ~/ 1000 + 300;
    final info = PairingInfoDto(
      pin: '482913',
      token: 'dG9rZW4tZmFrZQ',
      uri:
          'hfa://pair?v=0&h=192.168.1.10&p=${settings.port}'
          '&id=AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE'
          '&t=dG9rZW4tZmFrZQ&n=${Uri.encodeComponent(appInfo.deviceName)}',
      expiresAtUnix: expires,
    );
    pairing = info;
    if (_demo) {
      _timers.add(
        Timer(const Duration(seconds: 6), () {
          if (pairing == null || !_hubRunning) return;
          completePairing(deviceId: 'a1b2-c3d4-e5f6-0077', name: 'Pixel 9');
          addSource(
            source(
              streamId: _nextStreamId++,
              deviceName: 'Pixel 9',
              label: 'Device audio',
              platform: 'android',
            ),
          );
        }),
      );
    }
    return info;
  }

  @override
  Future<void> hubCancelPairing() async {
    calls.add('hubCancelPairing');
    pairing = null;
  }

  @override
  Stream<HubEventDto> hubEvents() {
    calls.add('hubEvents');
    return _hubEvents.stream;
  }

  // ----------------------------------------------------------------- sender

  @override
  Stream<DiscoveryEventDto> discoverHubs() {
    calls.add('discoverHubs');
    _discovery?.close();
    late final StreamController<DiscoveryEventDto> controller;
    controller = StreamController<DiscoveryEventDto>(
      onListen: () {
        for (final hub in discoverableHubs) {
          controller.add(DiscoveryEventDto.found(hub));
        }
      },
    );
    _discovery = controller;
    return controller.stream;
  }

  @override
  Future<void> stopDiscovery() async {
    calls.add('stopDiscovery');
    final discovery = _discovery;
    _discovery = null;
    await discovery?.close();
  }

  @override
  Future<List<CaptureAppDto>> listCaptureApps() async {
    calls.add('listCaptureApps');
    return List.of(captureApps);
  }

  static const _liveStates = {
    'connecting',
    'pairing',
    'streaming',
    'reconnecting',
  };

  @override
  Future<void> senderStart(SenderStartDto request) async {
    calls.add('senderStart');
    if (_liveStates.contains(_senderStatus.state)) {
      throw const HfaApiException('a sender is already running');
    }
    lastSenderStart = request;
    final hubId = request.hubDeviceId;
    final known =
        request.hubKey != null ||
        (hubId != null && trusted.any((p) => p.deviceId == hubId));
    final secret = request.pairingSecret?.trim() ?? '';
    final hubName =
        discoverableHubs
            .where((h) => h.deviceId == hubId)
            .map((h) => h.name)
            .firstOrNull ??
        (request.hubHost.isEmpty ? 'Hub' : request.hubHost);
    emitSenderStatus(
      const SenderStatusDto(
        state: 'connecting',
        bitrate: 0,
        lossPct: 0,
        rttMs: 0,
        levelDb: -120,
      ),
    );
    if (!known && secret.isEmpty) {
      emitSenderStatus(
        SenderStatusDto(
          state: 'failed',
          error: 'pairing required: enter the PIN shown on $hubName',
          bitrate: 0,
          lossPct: 0,
          rttMs: 0,
          levelDb: -120,
        ),
      );
      return;
    }
    if (!connectAutomatically) return;
    emitSenderStatus(
      SenderStatusDto(
        state: 'streaming',
        hubName: hubName,
        bitrate: settings.bitrate,
        lossPct: 0.2,
        rttMs: 4.5,
        levelDb: -20,
      ),
    );
    if (_demo) _startDemoTimer();
  }

  @override
  Future<void> senderStop() async {
    calls.add('senderStop');
    emitSenderStatus(idleStatus);
  }

  @override
  Future<SenderStatusDto> senderStatus() async => _senderStatus;

  @override
  Stream<SenderStatusDto> senderEvents() {
    calls.add('senderEvents');
    return Stream<SenderStatusDto>.multi((controller) {
      controller.add(_senderStatus);
      final sub = _senderEvents.stream.listen(
        controller.add,
        onError: controller.addError,
        onDone: controller.close,
      );
      controller.onCancel = sub.cancel;
    }, isBroadcast: true);
  }

  // ------------------------------------------------------------------- demo

  void _seedDemoSources() {
    _sources[_nextStreamId] = source(
      streamId: _nextStreamId++,
      deviceName: 'Work laptop',
      label: 'Zoom',
      platform: 'windows',
      priority: true,
    );
    _sources[_nextStreamId] = source(
      streamId: _nextStreamId++,
      deviceName: 'Phone',
      label: 'Device audio',
      platform: 'android',
      gain: 0.8,
    );
  }

  void _startDemoTimer() {
    if (_demoTicker?.isActive ?? false) return;
    final timer = Timer.periodic(const Duration(milliseconds: 250), (_) {
      _tick++;
      if (_hubRunning) {
        for (final id in _sources.keys.toList()) {
          final s = _sources[id];
          if (s == null) continue;
          final wave = math.sin(_tick / 3 + id) * 12;
          final level = s.muted ? -120.0 : -24 + wave;
          _updateSource(id, (s) => _copySource(s, levelDb: level));
        }
      }
      final status = _senderStatus;
      if (status.state == 'streaming') {
        emitSenderStatus(
          SenderStatusDto(
            state: status.state,
            hubName: status.hubName,
            bitrate: status.bitrate,
            lossPct: (math.sin(_tick / 7) + 1) * 0.4,
            rttMs: 3 + (math.cos(_tick / 5) + 1) * 2,
            levelDb: -22 + math.sin(_tick / 2) * 10,
          ),
        );
      }
    });
    _demoTicker = timer;
    _timers.add(timer);
  }
}
