/// The app's view of the Rust core.
///
/// Every screen and provider talks to [HfaApi] instead of the generated
/// flutter_rust_bridge functions, so the UI can run against [RustHfaApi] (the
/// real core) or [FakeHfaApi] (widget tests and the `--dart-define=HFA_FAKE=true`
/// demo mode). The DTO types are the generated ones and are re-exported here.
library;

import 'package:flutter_rust_bridge/flutter_rust_bridge.dart'
    show AnyhowException, PanicException;

import '../rust/api/app.dart';
import '../rust/api/hub.dart';
import '../rust/api/sender.dart';

export '../rust/api/app.dart'
    show AppInfo, CapabilitiesDto, PairingUriDto, SettingsDto, TrustedPeerDto;
export '../rust/api/hub.dart'
    show
        HubEventDto,
        HubEventDto_Error,
        HubEventDto_PairingCompleted,
        HubEventDto_PairingFailed,
        HubEventDto_SourceAdded,
        HubEventDto_SourceRemoved,
        HubEventDto_SourceUpdated,
        HubStatusDto,
        PairingInfoDto,
        SourceDto;
export '../rust/api/sender.dart'
    show
        CaptureAppDto,
        CaptureSourceDto,
        CaptureSourceDto_External,
        CaptureSourceDto_Process,
        CaptureSourceDto_System,
        CaptureSourceDto_SystemExcludingSelf,
        CaptureSourceDto_Tone,
        DiscoveryEventDto,
        DiscoveryEventDto_Found,
        DiscoveryEventDto_Lost,
        HubInfoDto,
        SenderStartDto,
        SenderStatusDto;
export 'fake_hfa_api.dart';
export 'rust_hfa_api.dart';

/// Everything the UI can ask of the core (docs/CONTRACTS.md §8.1, §8.5).
///
/// Methods mirror the generated `lib/src/rust/api/*.dart` functions one to
/// one; errors are thrown as-is (the real core throws `AnyhowException`, use
/// [describeError] to show them).
abstract class HfaApi {
  /// Loads settings, identity and trust store from [dataDir] (idempotent).
  /// [deviceName] is only used on the very first run.
  Future<AppInfo> initApp({required String dataDir, String? deviceName});

  /// The current settings.
  Future<SettingsDto> getSettings();

  /// Validates and saves [settings]; running engines keep theirs until restarted.
  Future<void> updateSettings(SettingsDto settings);

  /// Names of the available output devices.
  Future<List<String>> listOutputDevices();

  /// Devices paired with this one.
  Future<List<TrustedPeerDto>> trustedPeers();

  /// Removes a paired device.
  Future<void> forgetPeer(String deviceId);

  /// Parses an `hfa://pair?...` URI (from a QR code or a pasted link).
  Future<PairingUriDto> parsePairingUri(String uri);

  /// Starts the hub (idempotent).
  Future<HubStatusDto> hubStart();

  /// Stops the hub (idempotent).
  Future<void> hubStop();

  /// Current hub status.
  Future<HubStatusDto> hubStatus();

  /// Current hub streams.
  Future<List<SourceDto>> hubSources();

  /// Sets a stream's linear gain (0..=4).
  Future<void> hubSetGain(int streamId, double gain);

  /// Mutes or unmutes a stream.
  Future<void> hubSetMuted(int streamId, bool muted);

  /// Marks a stream as priority (it ducks the others).
  Future<void> hubSetPriority(int streamId, bool priority);

  /// Sets the master linear gain (0..=4).
  Future<void> hubSetMasterGain(double gain);

  /// Opens a pairing window (PIN + QR URI).
  Future<PairingInfoDto> hubStartPairing();

  /// The open pairing window, or null (none opened, cancelled, expired, used,
  /// or closed by the core after 5 failed attempts; null while stopped).
  Future<PairingInfoDto?> hubPairingStatus();

  /// Closes the pairing window.
  Future<void> hubCancelPairing();

  /// Hub events; the subscription survives hub restarts.
  Stream<HubEventDto> hubEvents();

  /// Browses for hubs until [stopDiscovery] or until the stream is cancelled.
  Stream<DiscoveryEventDto> discoverHubs();

  /// Stops browsing.
  Future<void> stopDiscovery();

  /// Processes that can be captured on their own.
  Future<List<CaptureAppDto>> listCaptureApps();

  /// Starts streaming to a hub.
  Future<void> senderStart(SenderStartDto request);

  /// Stops the sender (idempotent).
  Future<void> senderStop();

  /// Current sender status.
  Future<SenderStatusDto> senderStatus();

  /// Sender status updates: the current status first, then every change.
  Stream<SenderStatusDto> senderEvents();
}

/// The status of a sender that is not running (what `senderStatus` returns
/// before any sender started).
const idleSenderStatus = SenderStatusDto(
  state: 'idle',
  bitrate: 0,
  lossPct: 0,
  rttMs: 0,
  levelDb: -120,
  hubGain: 1,
  hubMuted: false,
  hubPriority: false,
);

/// A short, human-readable message for an error thrown by [HfaApi] or the
/// platform channel.
String describeError(Object error) {
  return switch (error) {
    AnyhowException(:final message) => _firstLine(message),
    // frb appends the Rust backtrace to the panic message.
    PanicException(:final message) =>
      'Internal error: ${_firstLine(message.split('Backtrace [').first)}',
    HfaApiException(:final message) => message,
    StateError(:final message) => message,
    ArgumentError(:final message) => '$message',
    _ => error.toString(),
  };
}

/// anyhow messages can carry a backtrace after the first line.
String _firstLine(String message) {
  final trimmed = message.trim();
  final newline = trimmed.indexOf('\n');
  return newline < 0 ? trimmed : trimmed.substring(0, newline).trim();
}

/// An error raised by an [HfaApi] implementation other than the Rust core
/// (the fake uses it to mimic the core's validation errors).
class HfaApiException implements Exception {
  /// Creates an exception with [message].
  const HfaApiException(this.message);

  /// Human-readable reason.
  final String message;

  @override
  String toString() => 'HfaApiException: $message';
}
