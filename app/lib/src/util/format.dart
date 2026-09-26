/// Small formatting helpers shared by the screens.
library;

import 'package:flutter/material.dart';

/// `-18 dB`, or `silent` at the core's -120 dBFS floor.
String formatDb(double db) {
  if (!db.isFinite || db <= -90) return 'silent';
  return '${db.round()} dB';
}

/// A linear gain as a percentage (`1.0` → `100%`).
String formatGain(double gain) => '${(gain * 100).round()}%';

/// `128 kbit/s`.
String formatBitrate(int bitsPerSecond) {
  if (bitsPerSecond <= 0) return '—';
  return '${(bitsPerSecond / 1000).round()} kbit/s';
}

/// `42 ms`.
String formatMs(double ms) => ms.isFinite ? '${ms.round()} ms' : '—';

/// `0.4%`.
String formatPercent(double pct) =>
    pct.isFinite ? '${pct.toStringAsFixed(pct < 10 ? 1 : 0)}%' : '—';

/// `4:59`.
String formatCountdown(int seconds) {
  final s = seconds < 0 ? 0 : seconds;
  return '${s ~/ 60}:${(s % 60).toString().padLeft(2, '0')}';
}

/// A PIN split for reading aloud: `482 913`.
String formatPin(String pin) =>
    pin.length == 6 ? '${pin.substring(0, 3)} ${pin.substring(3)}' : pin;

/// A date from Unix seconds: `2026-09-25`.
String formatUnixDate(int unixSeconds) {
  final d = DateTime.fromMillisecondsSinceEpoch(unixSeconds * 1000);
  String two(int v) => v.toString().padLeft(2, '0');
  return '${d.year}-${two(d.month)}-${two(d.day)}';
}

/// Local wall-clock time of [time] as `HH:mm`.
String formatClock(DateTime time) {
  final local = time.toLocal();
  String two(int v) => v.toString().padLeft(2, '0');
  return '${two(local.hour)}:${two(local.minute)}';
}

/// Display name of a core platform string.
String platformName(String platform) => switch (platform) {
  'windows' => 'Windows',
  'macos' => 'macOS',
  'linux' => 'Linux',
  'android' => 'Android',
  'ios' => 'iOS',
  '' => 'Unknown',
  _ => platform,
};

/// Icon of a core platform string.
IconData platformIcon(String platform) => switch (platform) {
  'windows' || 'linux' => Icons.desktop_windows_outlined,
  'macos' => Icons.laptop_mac_outlined,
  'android' => Icons.phone_android_outlined,
  'ios' => Icons.phone_iphone_outlined,
  _ => Icons.devices_other_outlined,
};

/// Human-readable sender state.
String senderStateLabel(String state) => switch (state) {
  'idle' => 'Not sending',
  'connecting' => 'Connecting…',
  'pairing' => 'Pairing…',
  'streaming' => 'Streaming',
  'reconnecting' => 'Reconnecting…',
  'stopped' => 'Stopped',
  'failed' => 'Failed',
  _ => state,
};
