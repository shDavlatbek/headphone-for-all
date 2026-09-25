import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:headphone_for_all/src/licenses.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();

  test('parses the generated notice format', () {
    final entries = parseRustLicenses(
      'a, b\n\nMIT text\nline 2\n${'-' * 80}\nlibopus\n\nBSD text\n',
    );
    expect(entries, hasLength(2));
    expect(entries[0].packages, ['a', 'b']);
    expect(
      entries[0].paragraphs.map((p) => p.text).join('\n'),
      contains('line 2'),
    );
    expect(entries[1].packages, ['libopus']);
  });

  test('the bundled notice covers libopus and the Rust crates', () {
    final text = File(rustLicensesAsset).readAsStringSync();
    final entries = parseRustLicenses(text);
    final packages = {for (final e in entries) ...e.packages};
    // libopus is linked statically: its BSD notice must ship (ROADMAP).
    expect(packages, containsAll(['libopus', 'opusic-sys']));
    expect(
      entries
          .firstWhere((e) => e.packages.contains('libopus'))
          .paragraphs
          .map((p) => p.text)
          .join('\n'),
      contains('Redistribution and use in source and binary forms'),
    );
    expect(packages, containsAll(['tokio', 'flutter_rust_bridge', 'snow']));
    expect(entries.length, greaterThan(50));
  });

  test('the licence page gets the Rust entries from the asset', () async {
    registerRustLicenses();
    final packages = <String>{};
    await for (final entry in LicenseRegistry.licenses) {
      packages.addAll(entry.packages);
    }
    expect(packages, contains('libopus'));
  });
}
