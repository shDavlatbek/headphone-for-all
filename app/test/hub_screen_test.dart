import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:headphone_for_all/src/api/hfa_api.dart';
import 'package:headphone_for_all/src/state/hub_controller.dart';
import 'package:headphone_for_all/src/state/navigation.dart';
import 'package:qr_flutter/qr_flutter.dart';

import 'package:flutter/services.dart';

import 'helpers.dart';

FakeHfaApi fakeWithSources() => FakeHfaApi()
  ..addSource(
    FakeHfaApi.source(streamId: 1, deviceName: 'Work laptop', label: 'Zoom'),
  )
  ..addSource(
    FakeHfaApi.source(
      streamId: 2,
      deviceName: 'Phone',
      label: 'Device audio',
      platform: 'android',
      active: false,
    ),
  );

Future<void> startHub(WidgetTester tester) async {
  await tester.tap(find.byKey(const Key('hub-toggle')));
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('start shows the live sources; stop clears them', (tester) async {
    final fake = fakeWithSources();
    final native = RecordingNativeChannel();
    await pumpApp(tester, fake, native: native, section: AppSection.hub);
    expect(find.text('Hub is off'), findsOneWidget);
    expect(find.byKey(const Key('source-1')), findsNothing);

    await startHub(tester);
    expect(find.text('Hub is on'), findsOneWidget);
    expect(find.text('Work laptop'), findsOneWidget);
    expect(find.text('Zoom'), findsOneWidget);
    expect(find.text('Device audio · idle'), findsOneWidget);
    expect(find.text('Sources (2)'), findsOneWidget);
    expect(native.calls, contains('startHubService'));

    await tester.tap(find.byKey(const Key('hub-toggle')));
    await tester.pumpAndSettle();
    expect(find.text('Hub is off'), findsOneWidget);
    expect(find.text('Work laptop'), findsNothing);
    expect(native.calls, contains('stopHubService'));
    await unmount(tester);
  });

  testWidgets('sources follow hub events', (tester) async {
    final fake = fakeWithSources();
    await pumpApp(tester, fake, section: AppSection.hub);
    await startHub(tester);

    fake.addSource(FakeHfaApi.source(streamId: 3, deviceName: 'Tablet'));
    await tester.pumpAndSettle();
    expect(find.text('Tablet'), findsOneWidget);

    fake.emitHubEvent(
      HubEventDto.sourceUpdated(
        FakeHfaApi.source(streamId: 3, deviceName: 'Tablet', levelDb: -6),
      ),
    );
    await tester.pumpAndSettle();
    expect(find.text('level -6 dB'), findsOneWidget);

    fake.removeSource(3);
    await tester.pumpAndSettle();
    expect(find.text('Tablet'), findsNothing);
    await unmount(tester);
  });

  testWidgets('the volume slider calls setGain', (tester) async {
    final fake = fakeWithSources();
    await pumpApp(tester, fake, section: AppSection.hub);
    await startHub(tester);

    // Drag the thumb of source 1 from 100% (the middle) to the far left.
    final slider = find.byKey(const Key('gain-1'));
    await tester.drag(slider, const Offset(-600, 0));
    await tester.pumpAndSettle();

    expect(fake.calls, contains('hubSetGain'));
    expect(fake.sources[1]!.gain, 0);
    expect(fake.sources[2]!.gain, 1);
    expect(find.text('0%'), findsOneWidget);
    await unmount(tester);
  });

  testWidgets('mute and priority toggles reach the core', (tester) async {
    final fake = fakeWithSources();
    await pumpApp(tester, fake, section: AppSection.hub);
    await startHub(tester);

    await tester.tap(find.byKey(const Key('mute-1')));
    await tester.pumpAndSettle();
    expect(fake.sources[1]!.muted, isTrue);
    expect(find.byIcon(Icons.volume_off), findsOneWidget);

    await tester.tap(find.byKey(const Key('priority-2')));
    await tester.pumpAndSettle();
    expect(fake.sources[2]!.priority, isTrue);
    expect(find.byIcon(Icons.star), findsOneWidget);

    await tester.tap(find.byKey(const Key('mute-1')));
    await tester.pumpAndSettle();
    expect(fake.sources[1]!.muted, isFalse);
    await unmount(tester);
  });

  testWidgets('master volume is sent when released', (tester) async {
    final fake = fakeWithSources();
    await pumpApp(tester, fake, section: AppSection.hub);
    await startHub(tester);
    await tester.drag(
      find.byKey(const Key('master-gain')),
      const Offset(2000, 0),
    );
    await tester.pumpAndSettle();
    expect(fake.masterGain, 2);
    expect(find.text('200%'), findsOneWidget);
    await unmount(tester);
  });

  testWidgets('pairing sheet shows the PIN and QR, then the result', (
    tester,
  ) async {
    final fake = fakeWithSources();
    await pumpApp(tester, fake, section: AppSection.hub);
    await startHub(tester);

    await tester.tap(find.byKey(const Key('pair-button')));
    await tester.pumpAndSettle();
    expect(find.text('482 913'), findsOneWidget);
    expect(find.byType(QrImageView), findsOneWidget);
    final qr = tester.widget<QrImageView>(find.byKey(const Key('pairing-qr')));
    expect(qr, isNotNull);
    expect(fake.pairing?.uri, startsWith('hfa://pair?'));
    expect(
      find.textContaining(RegExp(r'Expires in [45]:\d\d')),
      findsOneWidget,
    );
    // For "Add by address" where the QR code cannot be used.
    expect(
      tester
          .widget<SelectableText>(find.byKey(const Key('pairing-address')))
          .data,
      '192.168.1.10:${fake.settings.port}',
    );

    fake.emitHubEvent(const HubEventDto.pairingFailed(reason: 'wrong PIN'));
    await tester.pumpAndSettle();
    expect(find.text('A pairing attempt failed: wrong PIN'), findsOneWidget);
    expect(find.text('482 913'), findsOneWidget);

    fake.completePairing(deviceId: 'p1', name: 'Pixel 9');
    await tester.pumpAndSettle();
    expect(find.text('Paired with Pixel 9'), findsOneWidget);
    await tester.tap(find.text('Done'));
    await tester.pumpAndSettle();
    expect(find.text('Paired with Pixel 9'), findsNothing);
    await unmount(tester);
  });

  testWidgets('closing the pairing sheet cancels the window', (tester) async {
    final fake = fakeWithSources();
    await pumpApp(tester, fake, section: AppSection.hub);
    await startHub(tester);
    await tester.tap(find.byKey(const Key('pair-button')));
    await tester.pumpAndSettle();
    expect(fake.pairing, isNotNull);

    await tester.tapAt(const Offset(10, 10)); // the barrier
    await tester.pumpAndSettle();
    expect(fake.calls, contains('hubCancelPairing'));
    expect(fake.pairing, isNull);
    await unmount(tester);
  });

  testWidgets('a hub stopped while pairing ends the sheet\'s wait', (
    tester,
  ) async {
    final fake = fakeWithSources();
    final container = await pumpApp(tester, fake, section: AppSection.hub);
    await startHub(tester);
    await tester.tap(find.byKey(const Key('pair-button')));
    await tester.pumpAndSettle();
    expect(find.text('482 913'), findsOneWidget);

    // E.g. from the desktop tray.
    await container.read(hubControllerProvider.notifier).stop();
    await tester.pumpAndSettle();
    expect(find.text('The hub stopped'), findsOneWidget);
    expect(find.byType(CircularProgressIndicator), findsNothing);
    await tester.tap(find.text('Close'));
    await tester.pumpAndSettle();
    expect(find.text('The hub stopped'), findsNothing);
    await unmount(tester);
  });

  testWidgets('hub errors are shown in a snack bar and kept on the hub', (
    tester,
  ) async {
    final fake = fakeWithSources();
    await pumpApp(tester, fake, section: AppSection.hub);
    await startHub(tester);
    fake.emitHubEvent(const HubEventDto.error(message: 'output device lost'));
    await tester.pump();
    await tester.pump();
    expect(
      find.descendant(
        of: find.byType(SnackBar),
        matching: find.text('output device lost'),
      ),
      findsOneWidget,
    );
    expect(
      find.descendant(
        of: find.byKey(const Key('hub-error')),
        matching: find.text('output device lost'),
      ),
      findsOneWidget,
    );
    await tester.tap(find.byKey(const Key('hub-error-dismiss')));
    await tester.pumpAndSettle();
    expect(find.byKey(const Key('hub-error')), findsNothing);
    await unmount(tester);
  });

  testWidgets(
    'an error raised on another section is announced and survives the poll',
    (tester) async {
      final fake = fakeWithSources();
      final container = await pumpApp(tester, fake);
      await container.read(hubControllerProvider.notifier).start();
      await tester.pumpAndSettle();

      fake.emitHubEvent(
        const HubEventDto.error(
          message: 'output device failed: headphone disconnected',
        ),
      );
      await tester.pump();
      await tester.pump();
      // Shown on Home, where the hub screen's own listener never was.
      expect(
        find.text('output device failed: headphone disconnected'),
        findsOneWidget,
      );
      // The source poll and source updates no longer wipe it.
      await container.read(hubControllerProvider.notifier).refreshSources();
      fake.addSource(FakeHfaApi.source(streamId: 3, deviceName: 'Tablet'));
      await tester.pumpAndSettle();
      expect(
        container.read(hubControllerProvider).error,
        'output device failed: headphone disconnected',
      );

      container.read(sectionProvider.notifier).select(AppSection.hub);
      await tester.pumpAndSettle();
      expect(
        find.descendant(
          of: find.byKey(const Key('hub-error')),
          matching: find.textContaining('headphone disconnected'),
        ),
        findsOneWidget,
      );
      await unmount(tester);
    },
  );

  testWidgets('a successful start clears an earlier start failure', (
    tester,
  ) async {
    final fake = _FlakyStartApi();
    await pumpApp(tester, fake, section: AppSection.hub);
    await startHub(tester);
    expect(find.byKey(const Key('hub-error')), findsOneWidget);
    expect(find.text('Hub is off'), findsOneWidget);
    await startHub(tester);
    expect(find.text('Hub is on'), findsOneWidget);
    expect(find.byKey(const Key('hub-error')), findsNothing);
    await unmount(tester);
  });

  testWidgets('the header lists the addresses to type into a sender', (
    tester,
  ) async {
    final fake = FakeHfaApi();
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
    await pumpApp(tester, fake, section: AppSection.hub);
    expect(find.byKey(const Key('hub-address-0')), findsNothing);
    await startHub(tester);
    expect(find.text('192.168.1.10:47810'), findsOneWidget);
    expect(find.text('[fd00::10]:47810'), findsOneWidget);
    expect(find.byKey(const Key('not-discoverable')), findsNothing);
    await tester.tap(find.byKey(const Key('copy-hub-address-1')));
    await tester.pumpAndSettle();
    expect(copied, '[fd00::10]:47810');
    expect(find.text('Copied [fd00::10]:47810'), findsOneWidget);
    await unmount(tester);
  });

  testWidgets('a hub that cannot be announced says why', (tester) async {
    final fake = FakeHfaApi()
      ..advertiseError = 'multicast is not available on this network';
    await pumpApp(tester, fake, section: AppSection.hub);
    await startHub(tester);
    expect(find.byKey(const Key('not-discoverable')), findsOneWidget);
    expect(
      find.text('Not discoverable — senders must add this hub by address'),
      findsOneWidget,
    );
    expect(
      find.text('Reason: multicast is not available on this network'),
      findsOneWidget,
    );
    // The addresses to add it by are right there.
    expect(find.byKey(const Key('hub-address-0')), findsOneWidget);
    await unmount(tester);
  });

  testWidgets('the master slider shows the gain the core saved', (
    tester,
  ) async {
    final fake = FakeHfaApi()..masterGain = 0.5;
    await pumpApp(tester, fake, section: AppSection.hub);
    // Before any start (the core applies it when the hub starts).
    expect(find.text('50%'), findsOneWidget);
    final slider = tester.widget<Slider>(find.byKey(const Key('master-gain')));
    expect(slider.value, 0.5);
    await startHub(tester);
    expect(find.text('50%'), findsOneWidget);
    await unmount(tester);
  });
}

/// The first hub start fails (no output device), later ones work.
class _FlakyStartApi extends FakeHfaApi {
  var _failed = false;

  @override
  Future<HubStatusDto> hubStart() async {
    if (!_failed) {
      _failed = true;
      throw const HfaApiException('no output device');
    }
    return super.hubStart();
  }
}
