import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'package:headphone_for_all/main.dart';
import 'package:headphone_for_all/src/rust/api/app.dart';

const _info = AppInfo(
  deviceId: 'ab12-cd34-ef56-7890',
  deviceName: 'Desk',
  platform: 'linux',
  version: '0.1.0',
  capabilities: CapabilitiesDto(
    systemMix: true,
    perApp: true,
    mutesLocalOutput: false,
    externalOnly: false,
    notes: 'PipeWire',
  ),
);

void main() {
  testWidgets('shows the device info once the core is ready', (tester) async {
    await tester.pumpWidget(HfaApp(appInfo: Future.value(_info)));
    expect(find.byType(CircularProgressIndicator), findsOneWidget);

    await tester.pumpAndSettle();
    expect(find.text('Headphone for All'), findsOneWidget);
    expect(find.text('Desk'), findsOneWidget);
    expect(find.text('ab12-cd34-ef56-7890'), findsOneWidget);
    expect(find.text('PipeWire'), findsOneWidget);
  });

  testWidgets('shows the error when the core fails', (tester) async {
    final info = Completer<AppInfo>();
    await tester.pumpWidget(HfaApp(appInfo: info.future));
    info.completeError(Exception('no data dir'));
    await tester.pumpAndSettle();
    expect(find.textContaining('no data dir'), findsOneWidget);
    expect(find.byIcon(Icons.error_outline), findsOneWidget);
  });
}
