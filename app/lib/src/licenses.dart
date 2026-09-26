/// Licences of the Rust core for the licence page.
///
/// `showLicensePage` lists what [LicenseRegistry] holds, which Flutter fills
/// with the Dart and Flutter packages only. The Rust core links libopus
/// (BSD-3-Clause, whose notice must ship with binaries) and many crates;
/// `tool/gen_rust_licenses.py` collects their licences into
/// [rustLicensesAsset], which [registerRustLicenses] adds.
library;

import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

/// The generated notice file (see `tool/gen_rust_licenses.py`).
const rustLicensesAsset = 'assets/licenses/rust.txt';

/// The line between two entries of [rustLicensesAsset].
final _separator = '\n${'-' * 80}\n';

/// Parses [rustLicensesAsset]: entries separated by a line of 80 '-', each
/// a line of comma-separated package names, an empty line and the text.
List<LicenseEntry> parseRustLicenses(String text) {
  final entries = <LicenseEntry>[];
  for (final block in text.replaceAll('\r\n', '\n').split(_separator)) {
    final trimmed = block.trim();
    final newline = trimmed.indexOf('\n');
    if (newline < 0) continue;
    final packages = [
      for (final name in trimmed.substring(0, newline).split(','))
        if (name.trim().isNotEmpty) name.trim(),
    ];
    final body = trimmed.substring(newline + 1).trim();
    if (packages.isEmpty || body.isEmpty) continue;
    entries.add(LicenseEntryWithLineBreaks(packages, body));
  }
  return entries;
}

/// Adds the Rust core's licences to [LicenseRegistry] (read when the licence
/// page opens). A missing asset only loses these entries.
void registerRustLicenses({AssetBundle? bundle}) {
  LicenseRegistry.addLicense(() async* {
    final String text;
    try {
      text = await (bundle ?? rootBundle).loadString(rustLicensesAsset);
    } catch (e) {
      debugPrint('Rust licences: $e');
      return;
    }
    yield* Stream.fromIterable(parseRustLicenses(text));
  });
}
