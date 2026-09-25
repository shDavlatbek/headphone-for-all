import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:headphone_for_all/src/api/hfa_api.dart';
import 'package:headphone_for_all/src/bootstrap.dart';
import 'package:headphone_for_all/src/state/navigation.dart';

import 'helpers.dart';

void main() {
  testWidgets('shows both roles, the device and its capabilities', (
    tester,
  ) async {
    final fake = FakeHfaApi(platform: 'macos', deviceName: 'Studio Mac');
    await pumpApp(tester, fake);

    expect(find.text('Headphone is connected here'), findsOneWidget);
    expect(find.text("Send this device's audio"), findsOneWidget);
    expect(find.byKey(const Key('device-name-label')), findsOneWidget);
    expect(find.text('Studio Mac'), findsOneWidget);
    expect(find.text('Everything this device plays'), findsOneWidget);
    expect(find.text('Single apps'), findsOneWidget);
    expect(
      find.text("Captured audio is muted on this device's own speakers"),
      findsOneWidget,
    );
    expect(find.byKey(const Key('capability-notes')), findsOneWidget);
    await unmount(tester);
  });

  testWidgets('Android explains its capture limits', (tester) async {
    await pumpApp(tester, FakeHfaApi(platform: 'android'));
    expect(find.textContaining('Playback of other apps'), findsOneWidget);
    expect(find.text('Everything this device plays'), findsNothing);
    await unmount(tester);
  });

  testWidgets('the role cards open the hub and sender screens', (tester) async {
    await pumpApp(tester, FakeHfaApi());
    await tester.tap(find.byKey(const Key('role-hub')));
    await tester.pumpAndSettle();
    expect(find.text('Hub is off'), findsOneWidget);

    await tester.tap(find.text('Home').last);
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('role-sender')));
    await tester.pumpAndSettle();
    expect(find.text('Not sending'), findsOneWidget);
    await unmount(tester);
  });

  testWidgets('wide windows use a rail, phones a bottom bar', (tester) async {
    await pumpApp(tester, FakeHfaApi(), size: const Size(1200, 800));
    expect(find.byType(NavigationRail), findsOneWidget);
    expect(find.byType(NavigationBar), findsNothing);
    await unmount(tester);

    await pumpApp(tester, FakeHfaApi(), size: const Size(390, 844));
    expect(find.byType(NavigationRail), findsNothing);
    expect(find.byType(NavigationBar), findsOneWidget);
    await unmount(tester);
  });

  testWidgets('the running hub shows a badge on Home', (tester) async {
    final fake = FakeHfaApi();
    await pumpApp(tester, fake);
    await tester.tap(find.byKey(const Key('role-hub')));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('hub-toggle')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Home').last);
    await tester.pumpAndSettle();
    expect(find.text('Hub is on'), findsOneWidget);
    await unmount(tester);
  });

  for (final (platform, size) in const [
    ('android', Size(360, 740)),
    ('ios', Size(360, 740)),
    ('linux', Size(360, 740)),
    ('windows', Size(1280, 800)),
  ]) {
    testWidgets('every $platform screen lays out at ${size.width.toInt()} px', (
      tester,
    ) async {
      final fake = FakeHfaApi(
        platform: platform,
        discoverableHubs: const [
          HubInfoDto(
            deviceId: 'h1',
            name: 'A hub with a rather long name for a small phone',
            addrs: ['fe80::1234:5678:9abc:def0'],
            port: 47810,
            platform: 'windows',
            trusted: false,
          ),
        ],
        trusted: const [
          TrustedPeerDto(deviceId: 'p1', name: 'Tablet', pairedAtUnix: 0),
        ],
      )..addSource(FakeHfaApi.source(streamId: 1, label: 'A long app label'));
      final container = await pumpApp(tester, fake, size: size);
      for (final section in AppSection.values) {
        container.read(sectionProvider.notifier).select(section);
        await tester.pumpAndSettle();
        if (section == AppSection.hub) {
          await tester.tap(find.byKey(const Key('hub-toggle')));
          await tester.pumpAndSettle();
          expect(find.byKey(const Key('source-1')), findsOneWidget);
        }
      }
      await unmount(tester);
    });
  }

  testWidgets('the init error screen shows the reason and retries', (
    tester,
  ) async {
    var retried = 0;
    await tester.pumpWidget(
      InitErrorApp(error: 'libhfa_ffi.so not found', onRetry: () => retried++),
    );
    expect(find.text("Headphone for All couldn't start"), findsOneWidget);
    expect(find.text('libhfa_ffi.so not found'), findsOneWidget);
    await tester.tap(find.text('Try again'));
    expect(retried, 1);
  });
}
