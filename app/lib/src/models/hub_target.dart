import 'package:flutter/foundation.dart';

import '../api/hfa_api.dart';

/// Where a [HubTarget] came from.
enum HubOrigin {
  /// Announced on the LAN (mDNS).
  discovered,

  /// In the trust store but not seen on the LAN right now.
  paired,

  /// Typed in by address.
  manual,

  /// Scanned from a QR code or pasted as a pairing link.
  pairingLink,
}

/// A hub the sender can stream to, with everything `SenderStartDto` needs.
@immutable
class HubTarget {
  /// Creates a target.
  const HubTarget({
    required this.name,
    required this.origin,
    this.host = '',
    this.port = 0,
    this.deviceId,
    this.hubKey,
    this.pairingSecret,
    this.trusted = false,
    this.platform = '',
  });

  /// A hub from discovery: dialled by its first address, key pinned by id.
  /// [trusted] overrides the announcement's (possibly stale) `trusted` flag,
  /// e.g. when the trust store gained the hub after it was discovered.
  factory HubTarget.discovered(HubInfoDto hub, {bool? trusted}) => HubTarget(
    name: hub.name,
    origin: HubOrigin.discovered,
    host: hub.addrs.isEmpty ? '' : hub.addrs.first,
    port: hub.port,
    deviceId: hub.deviceId,
    trusted: trusted ?? hub.trusted,
    platform: hub.platform,
  );

  /// A paired device that is not currently discovered: the core looks it up
  /// over mDNS by id (no [address]), or it is dialled at its last known
  /// [address] (iOS, which cannot browse mDNS).
  factory HubTarget.paired(TrustedPeerDto peer, {HubAddress? address}) =>
      HubTarget(
        name: peer.name,
        origin: HubOrigin.paired,
        host: address?.host ?? '',
        port: address?.port ?? 0,
        deviceId: peer.deviceId,
        trusted: true,
      );

  /// A hub from a scanned QR code or pasted link: key and one-time token
  /// included, so no PIN is needed.
  factory HubTarget.fromPairingUri(PairingUriDto uri) => HubTarget(
    name: uri.name,
    origin: HubOrigin.pairingLink,
    host: uri.host,
    port: uri.port,
    deviceId: uri.hubDeviceId,
    hubKey: uri.hubId,
    pairingSecret: uri.token,
    trusted: true,
  );

  /// A hub typed in by address (PIN optional if it is already paired).
  factory HubTarget.manual({required String host, int port = 0, String? pin}) =>
      HubTarget(
        name: port == 0 ? host : '$host:$port',
        origin: HubOrigin.manual,
        host: host,
        port: port,
        pairingSecret: (pin == null || pin.trim().isEmpty) ? null : pin.trim(),
      );

  /// Display name.
  final String name;

  /// Where it came from.
  final HubOrigin origin;

  /// Host or IP; empty = find by [deviceId].
  final String host;

  /// Port; 0 = default.
  final int port;

  /// Hub device id (fingerprint), when known.
  final String? deviceId;

  /// Hub static key (base64url), from a pairing link.
  final String? hubKey;

  /// PIN or one-time token to pair with.
  final String? pairingSecret;

  /// This device already trusts the hub (or holds its key from a QR code).
  final bool trusted;

  /// Hub platform, when known.
  final String platform;

  /// Stable identity for selection.
  String get key => deviceId ?? '$host:$port';

  /// Human-readable address.
  String get address {
    if (host.isEmpty) return 'found by id on the network';
    final h = host.contains(':') ? '[$host]' : host;
    return port == 0 ? h : '$h:$port';
  }

  /// Starting needs a PIN first: the hub is known not to trust us and no
  /// secret is at hand. (A manual target may be paired already, so the core
  /// decides; it fails with "pairing required" if not.)
  bool get needsPin =>
      origin == HubOrigin.discovered && !trusted && pairingSecret == null;

  /// A copy with [pin] as the pairing secret.
  HubTarget withPin(String? pin) => HubTarget(
    name: name,
    origin: origin,
    host: host,
    port: port,
    deviceId: deviceId,
    hubKey: hubKey,
    pairingSecret: (pin == null || pin.trim().isEmpty) ? null : pin.trim(),
    trusted: trusted,
    platform: platform,
  );

  /// A copy after a successful pairing: trusted, the one-time secret
  /// dropped, and [deviceId] filled in when it was learnt.
  HubTarget asPaired({String? deviceId}) => HubTarget(
    name: name,
    origin: origin,
    host: host,
    port: port,
    deviceId: deviceId ?? this.deviceId,
    hubKey: hubKey,
    trusted: true,
    platform: platform,
  );

  /// The start request for [source] with [label].
  SenderStartDto toRequest(CaptureSourceDto source, {String label = ''}) {
    return SenderStartDto(
      hubHost: host,
      hubPort: port,
      hubDeviceId: deviceId,
      hubKey: hubKey,
      pairingSecret: pairingSecret,
      source: source,
      label: label,
    );
  }

  @override
  bool operator ==(Object other) =>
      other is HubTarget &&
      other.name == name &&
      other.origin == origin &&
      other.host == host &&
      other.port == port &&
      other.deviceId == deviceId &&
      other.hubKey == hubKey &&
      other.pairingSecret == pairingSecret &&
      other.trusted == trusted &&
      other.platform == platform;

  @override
  int get hashCode => Object.hash(
    name,
    origin,
    host,
    port,
    deviceId,
    hubKey,
    pairingSecret,
    trusted,
    platform,
  );
}

/// Where a hub was reached.
@immutable
class HubAddress {
  /// Creates an address.
  const HubAddress(this.host, this.port);

  /// Host or IP address.
  final String host;

  /// Port (0 = default).
  final int port;

  @override
  bool operator ==(Object other) =>
      other is HubAddress && other.host == host && other.port == port;

  @override
  int get hashCode => Object.hash(host, port);

  @override
  String toString() => port == 0 ? host : '$host:$port';
}

/// The selected [target] as it is known now: a discovered or paired hub is
/// looked up again by device id in [discovered] (visible hubs by id) and
/// [peers], so a new address, port or trust state is used; [addresses] are
/// the last known addresses of paired hubs that are not discovered (iOS).
/// A typed-in or scanned target, or one no longer known, is returned as is.
/// A PIN or token the user entered is kept.
HubTarget currentHubTarget(
  HubTarget target, {
  required Map<String, HubInfoDto> discovered,
  required List<TrustedPeerDto> peers,
  Map<String, HubAddress> addresses = const {},
}) {
  final id = target.deviceId;
  if (id == null ||
      (target.origin != HubOrigin.discovered &&
          target.origin != HubOrigin.paired)) {
    return target;
  }
  final hub = discovered[id];
  final peer = peers.where((p) => p.deviceId == id).firstOrNull;
  final HubTarget fresh;
  if (hub != null) {
    fresh = HubTarget.discovered(hub, trusted: hub.trusted || peer != null);
  } else if (peer != null) {
    fresh = HubTarget.paired(peer, address: addresses[id]);
  } else {
    return target;
  }
  return target.pairingSecret == null
      ? fresh
      : fresh.withPin(target.pairingSecret);
}

/// Whether [text] is a 6-digit pairing PIN.
bool isValidPin(String text) => RegExp(r'^\d{6}$').hasMatch(text.trim());
