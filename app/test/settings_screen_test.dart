import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:headphone_for_all/src/api/hfa_api.dart';
import 'package:headphone_for_all/src/models/hub_target.dart';
import 'package:headphone_for_all/src/state/app_prefs.dart';
import 'package:headphone_for_all/src/state/hub_controller.dart';
import 'package:headphone_for_all/src/state/sender_controller.dart';
import 'package:headphone_for_all/src/state/navigation.dart';

import 'package:flutter/services.dart';
import 'package:headphone_for_all/src/util/links.dart';

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
          pairedAsHub: true,
          pairedAsSender: true,
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

  testWidgets('the start-on-launch switch applies at once', (tester) async {
    final container = await pumpApp(
      tester,
      FakeHfaApi(),
      section: AppSection.settings,
    );
    expect(container.read(appPrefsProvider).startHubOnLaunch, isFalse);
    await tester.tap(find.byKey(const Key('start-hub-on-launch')));
    await tester.pumpAndSettle();
    expect(container.read(appPrefsProvider).startHubOnLaunch, isTrue);
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
          pairedAsHub: true,
          pairedAsSender: true,
        ),
        const TrustedPeerDto(
          deviceId: 'bbbb-2222',
          name: 'Tablet',
          pairedAtUnix: 1767225600,
          pairedAsHub: true,
          pairedAsSender: true,
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

  testWidgets('trusted devices show how they were paired', (tester) async {
    final fake = FakeHfaApi(
      trusted: [
        FakeHfaApi.peer(deviceId: 'hub-1', name: 'Desk PC'),
        FakeHfaApi.peer(
          deviceId: 'phone-1',
          name: 'Phone',
          asHub: false,
          asSender: true,
        ),
        FakeHfaApi.peer(deviceId: 'old-1', name: 'Old laptop', asSender: true),
      ],
    );
    await pumpApp(tester, fake, section: AppSection.settings);
    await tester.scrollUntilVisible(
      find.byKey(const Key('peer-old-1')),
      300,
      scrollable: find.byType(Scrollable).first,
    );
    expect(find.byKey(const Key('peer-hub-1-hub')), findsOneWidget);
    expect(find.byKey(const Key('peer-hub-1-sender')), findsNothing);
    expect(find.byKey(const Key('peer-phone-1-hub')), findsNothing);
    expect(find.byKey(const Key('peer-phone-1-sender')), findsOneWidget);
    expect(find.byKey(const Key('peer-old-1-hub')), findsOneWidget);
    expect(find.byKey(const Key('peer-old-1-sender')), findsOneWidget);
    await unmount(tester);
  });

  testWidgets('Linux switches starting at sign-in', (tester) async {
    final signIn = MemorySignInLauncher();
    await pumpApp(
      tester,
      FakeHfaApi(),
      section: AppSection.settings,
      signIn: signIn,
    );
    await tester.scrollUntilVisible(
      find.byKey(const Key('start-at-sign-in')),
      300,
      scrollable: find.byType(Scrollable).first,
    );
    expect(find.byKey(const Key('sign-in-hint')), findsNothing);
    await tester.tap(find.byKey(const Key('start-at-sign-in')));
    await tester.pumpAndSettle();
    expect(signIn.enabled, isTrue);
    expect(
      tester
          .widget<SwitchListTile>(find.byKey(const Key('start-at-sign-in')))
          .value,
      isTrue,
    );
    await tester.tap(find.byKey(const Key('start-at-sign-in')));
    await tester.pumpAndSettle();
    expect(signIn.enabled, isFalse);
    await unmount(tester);
  });

  testWidgets('Windows says where starting at sign-in is switched', (
    tester,
  ) async {
    await pumpApp(
      tester,
      FakeHfaApi(platform: 'windows'),
      section: AppSection.settings,
    );
    await tester.scrollUntilVisible(
      find.byKey(const Key('sign-in-hint')),
      300,
      scrollable: find.byType(Scrollable).first,
    );
    expect(find.byKey(const Key('start-at-sign-in')), findsNothing);
    expect(find.textContaining('installer'), findsOneWidget);
    await unmount(tester);
  });

  testWidgets('about opens the user guide; without a browser it copies', (
    tester,
  ) async {
    final links = RecordingLinkOpener();
    String? copied;
    tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
      SystemChannels.platform,
      (call) async {
        if (call.method == 'Clipboard.setData') {
          copied = (call.arguments as Map)['text'] as String?;
        }
        return null;
      },
    );
    addTearDown(
      () => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        SystemChannels.platform,
        null,
      ),
    );
    await pumpApp(
      tester,
      FakeHfaApi(),
      section: AppSection.about,
      links: links,
    );
    await tester.tap(find.byKey(const Key('help-guide')));
    await tester.pumpAndSettle();
    expect(links.opened, [
      Uri.parse(
        'https://github.com/shDavlatbek/headphone-for-all/blob/main/docs/USER_GUIDE.md',
      ),
    ]);
    expect(copied, isNull);

    links.opens = false;
    await tester.tap(find.byKey(const Key('help-guide')));
    await tester.pumpAndSettle();
    expect(copied, userGuideUrl);
    expect(find.textContaining('Link copied'), findsOneWidget);
    await unmount(tester);
  });
}
