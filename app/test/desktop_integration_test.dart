import 'package:flutter/services.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:headphone_for_all/src/api/hfa_api.dart';
import 'package:headphone_for_all/src/bootstrap.dart';
import 'package:headphone_for_all/src/platform/desktop_integration.dart';
import 'package:headphone_for_all/src/state/sender_controller.dart';
import 'package:tray_manager/tray_manager.dart' as tray;

import 'helpers.dart';

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  const channel = MethodChannel('window_manager');
  final messenger =
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger;
  late List<MethodCall> calls;

  setUp(() {
    calls = [];
    messenger.setMockMethodCallHandler(channel, (call) async {
      calls.add(call);
      return null;
    });
  });
  tearDown(() => messenger.setMockMethodCallHandler(channel, null));

  List<Object?> preventClose() => [
    for (final c in calls)
      if (c.method == 'setPreventClose')
        (c.arguments as Map<Object?, Object?>)['isPreventClose'],
  ];

  test('tray menu trigger per platform', () {
    // Linux exposes the DBusMenu only with the "clicked" trigger.
    expect(trayClickPolicy('linux'), (
      menuTrigger: tray.ContextMenuTrigger.clicked,
      clickTogglesWindow: false,
    ));
    expect(trayClickPolicy('macos'), (
      menuTrigger: tray.ContextMenuTrigger.clicked,
      clickTogglesWindow: false,
    ));
    expect(trayClickPolicy('windows'), (
      menuTrigger: tray.ContextMenuTrigger.rightClicked,
      clickTogglesWindow: true,
    ));
  });

  test('the window is hidden only into a tray that is shown', () {
    expect(
      closeActionFor(busy: true, trayUsable: true),
      CloseAction.hideToTray,
    );
    expect(closeActionFor(busy: true, trayUsable: false), CloseAction.quit);
    expect(closeActionFor(busy: false, trayUsable: true), CloseAction.quit);
    expect(closeActionFor(busy: false, trayUsable: false), CloseAction.quit);
  });

  test('the tray tells what runs and offers to stop sending', () {
    expect(trayTooltip(hubOn: false), 'Headphone for All');
    expect(
      trayTooltip(hubOn: true, sendingTo: 'Desk PC'),
      'Headphone for All — Hub on · Sending to Desk PC',
    );
    expect(
      trayTooltip(hubOn: false, hubError: 'output device lost'),
      'Headphone for All — Hub error: output device lost',
    );

    const idle = SenderState(status: idleSenderStatus);
    expect(traySendingTo(idle), isNull);
    expect(traySenderLabel(traySendingTo(idle)), 'Not sending');
    const live = SenderState(
      status: SenderStatusDto(
        state: 'streaming',
        hubName: 'Desk PC',
        bitrate: 128000,
        lossPct: 0,
        rttMs: 3,
        levelDb: -20,
      ),
    );
    expect(traySendingTo(live), 'Desk PC');
    expect(traySenderLabel(traySendingTo(live)), 'Stop sending to Desk PC');
  });

  test('the Linux tray host probe never throws', () async {
    // No session bus or no StatusNotifierWatcher here: "no host".
    expect(
      await linuxTrayHostAvailable().timeout(const Duration(seconds: 10)),
      isA<bool>(),
    );
  });

  test('startup leaves the close button alone', () async {
    await initDesktopWindow();
    expect(calls.map((c) => c.method), contains('ensureInitialized'));
    expect(preventClose(), isEmpty);
  });

  testWidgets('the error screen closes normally', (tester) async {
    await tester.pumpWidget(const InitErrorApp(error: 'boom'));
    expect(find.text('boom'), findsOneWidget);
    expect(preventClose(), isEmpty);
  });

  testWidgets('close is intercepted only while the integration is mounted', (
    tester,
  ) async {
    final fake = FakeHfaApi();
    await tester.pumpWidget(
      ProviderScope(
        overrides: overridesFor(fake),
        child: const DesktopIntegration(
          enabled: true,
          showTray: false,
          child: SizedBox.shrink(),
        ),
      ),
    );
    await tester.pump();
    expect(preventClose(), [true]);

    await tester.pumpWidget(const SizedBox.shrink());
    await tester.pump();
    expect(preventClose(), [true, false]);
  });
}
