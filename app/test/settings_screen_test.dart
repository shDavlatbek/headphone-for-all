import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:headphone_for_all/src/api/hfa_api.dart';
import 'package:headphone_for_all/src/models/hub_target.dart';
import 'package:headphone_for_all/src/state/hub_controller.dart';
import 'package:headphone_for_all/src/state/sender_controller.dart';
import 'package:headphone_for_all/src/state/navigation.dart';

import 'helpers.dart';

void main() {
  testWidgets('edits and saves the settings; the new name is used', (
    tester,
  ) async {
    final fake = FakeHfaApi(deviceName: 'Laptop');
    final container = await pumpApp(tester, fake, section: AppSection.settings);

    expect(find.text('Laptop'), findsOneWidget);
    expect(find.text('Jitter buffer: 20–200 ms'), findsOneWidget);
    await tester.enterText(find.byKey(const Key('device-name')), 'Kitchen');
    await tester.tap(find.text('20 ms'));
    await tester.tap(find.byKey(const Key('fec')));
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('bitrate')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('96 kbit/s').last);
    await tester.pumpAndSettle();

    await tester.tap(find.byKey(const Key('save-settings')));
    await tester.pumpAndSettle();
    expect(fake.settings.deviceName, 'Kitchen');
    expect(fake.settings.frameMs, 20);
    expect(fake.settings.fec, isFalse);
    expect(fake.settings.bitrate, 96000);
    expect(fake.settings.jitterMinMs, 20);
    expect(fake.settings.jitterMaxMs, 200);
    expect(find.text('Settings saved.'), findsOneWidget);

    container.read(sectionProvider.notifier).select(AppSection.home);
    await tester.pumpAndSettle();
    expect(find.text('Kitchen'), findsOneWidget);
    await unmount(tester);
  });

  testWidgets(
    'saving with the hub running offers a restart that works although the '
    'form was rebuilt',
    (tester) async {
      final fake = FakeHfaApi(deviceName: 'Laptop');
      final container = await pumpApp(
        tester,
        fake,
        section: AppSection.settings,
      );
      await container.read(hubControllerProvider.notifier).start();
      await tester.pumpAndSettle();
      expect(fake.calls.where((c) => c == 'hubStart'), hasLength(1));

      // A changed setting: the saved value differs, so the form (keyed by
      // it) is replaced while the snack bar is still shown.
      await tester.enterText(find.byKey(const Key('device-name')), 'Kitchen');
      await tester.tap(find.byKey(const Key('save-settings')));
      await tester.pumpAndSettle();
      expect(
        find.text('Saved. Restart the hub to apply the changes.'),
        findsOneWidget,
      );
      await tester.tap(find.text('Restart hub'));
      await tester.pumpAndSettle();
      expect(tester.takeException(), isNull);
      expect(fake.calls.where((c) => c == 'hubStart'), hasLength(2));
      expect(fake.calls, contains('hubStop'));
      expect(container.read(hubControllerProvider).running, isTrue);
      await unmount(tester);
    },
  );

  testWidgets('saving while sending says the sender needs a restart', (
    tester,
  ) async {
    final fake = FakeHfaApi(
      trusted: [
        const TrustedPeerDto(
          deviceId: 'hub-1',
          name: 'Desk',
          pairedAtUnix: 1700000000,
        ),
      ],
    );
    final container = await pumpApp(tester, fake, section: AppSection.settings);
    final sender = container.read(senderControllerProvider.notifier);
    sender.selectTarget(
      const HubTarget(
        name: 'Desk',
        origin: HubOrigin.manual,
        host: '10.0.0.2',
        deviceId: 'hub-1',
        trusted: true,
      ),
    );
    await sender.start();
    await tester.pumpAndSettle();
    expect(container.read(senderControllerProvider).isLive, isTrue);

    await tester.tap(find.byKey(const Key('fec')));
    await tester.tap(find.byKey(const Key('save-settings')));
    await tester.pumpAndSettle();
    expect(
      find.text('Saved. Stop and start sending to apply the changes.'),
      findsOneWidget,
    );
    final starts = fake.calls.where((c) => c == 'senderStart').length;
    await tester.tap(find.text('Restart sending'));
    await tester.pumpAndSettle();
    expect(tester.takeException(), isNull);
    expect(fake.calls.where((c) => c == 'senderStart'), hasLength(starts + 1));
    expect(container.read(senderControllerProvider).isLive, isTrue);
    await unmount(tester);
  });

  testWidgets('an empty name is refused before reaching the core', (
    tester,
  ) async {
    final fake = FakeHfaApi();
    await pumpApp(tester, fake, section: AppSection.settings);
    await tester.enterText(find.byKey(const Key('device-name')), '   ');
    await tester.tap(find.byKey(const Key('save-settings')));
    await tester.pumpAndSettle();
    expect(find.text('Use 1 to 64 characters'), findsOneWidget);
    expect(fake.calls, isNot(contains('updateSettings')));
    await unmount(tester);
  });

  testWidgets('an out-of-range port is refused, not truncated', (tester) async {
    final fake = FakeHfaApi();
    await pumpApp(tester, fake, section: AppSection.settings);
    await tester.enterText(find.byKey(const Key('port')), '70000');
    await tester.tap(find.byKey(const Key('save-settings')));
    await tester.pumpAndSettle();
    expect(find.text('Port must be 1–65535'), findsOneWidget);
    expect(fake.calls, isNot(contains('updateSettings')));

    await tester.enterText(find.byKey(const Key('port')), '65535');
    await tester.tap(find.byKey(const Key('save-settings')));
    await tester.pumpAndSettle();
    expect(find.text('Port must be 1–65535'), findsNothing);
    expect(fake.settings.port, 65535);
    await unmount(tester);
  });

  testWidgets('desktop offers the output device; phones do not', (
    tester,
  ) async {
    final fake = FakeHfaApi(outputDevices: ['Speakers', 'USB DAC']);
    await pumpApp(tester, fake, section: AppSection.settings);
    expect(find.text('System default output'), findsOneWidget);
    await tester.tap(find.byKey(const Key('output-device')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('USB DAC').last);
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('save-settings')));
    await tester.pumpAndSettle();
    expect(fake.settings.outputDevice, 'USB DAC');
    await unmount(tester);

    await pumpApp(
      tester,
      FakeHfaApi(platform: 'android'),
      section: AppSection.settings,
    );
    expect(find.byKey(const Key('output-device')), findsNothing);
    await unmount(tester);
  });

  testWidgets('trusted devices can be forgotten after confirming', (
    tester,
  ) async {
    final fake = FakeHfaApi(
      trusted: [
        const TrustedPeerDto(
          deviceId: 'aaaa-1111',
          name: 'Phone',
          pairedAtUnix: 1767225600,
        ),
        const TrustedPeerDto(
          deviceId: 'bbbb-2222',
          name: 'Tablet',
          pairedAtUnix: 1767225600,
        ),
      ],
    );
    await pumpApp(tester, fake, section: AppSection.settings);
    expect(find.text('Phone'), findsOneWidget);
    expect(find.text('Tablet'), findsOneWidget);

    await tester.tap(find.byKey(const Key('forget-aaaa-1111')));
    await tester.pumpAndSettle();
    expect(find.text('Forget Phone?'), findsOneWidget);
    await tester.tap(find.text('Cancel'));
    await tester.pumpAndSettle();
    expect(fake.trusted, hasLength(2));

    await tester.tap(find.byKey(const Key('forget-aaaa-1111')));
    await tester.pumpAndSettle();
    await tester.tap(find.widgetWithText(FilledButton, 'Forget'));
    await tester.pumpAndSettle();
    expect(fake.trusted.map((p) => p.deviceId), ['bbbb-2222']);
    expect(find.text('Phone'), findsNothing);
    expect(find.text('Forgot Phone.'), findsOneWidget);
    await unmount(tester);
  });

  testWidgets('about shows the version and the licences', (tester) async {
    await pumpApp(tester, FakeHfaApi(), section: AppSection.about);
    expect(find.text('Version 0.1.0'), findsOneWidget);
    await tester.tap(find.byKey(const Key('licences')));
    await tester.pumpAndSettle();
    expect(find.byType(LicensePage), findsOneWidget);
    await unmount(tester);
  });
}
