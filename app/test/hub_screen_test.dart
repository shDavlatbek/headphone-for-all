import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:headphone_for_all/src/api/hfa_api.dart';
import 'package:headphone_for_all/src/state/navigation.dart';
import 'package:qr_flutter/qr_flutter.dart';

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

  testWidgets('hub errors are shown in a snack bar', (tester) async {
    final fake = fakeWithSources();
    await pumpApp(tester, fake, section: AppSection.hub);
    await startHub(tester);
    fake.emitHubEvent(const HubEventDto.error(message: 'output device lost'));
    await tester.pump();
    await tester.pump();
    expect(find.text('output device lost'), findsOneWidget);
    await unmount(tester);
  });
}
