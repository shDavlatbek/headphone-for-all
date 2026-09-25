import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:headphone_for_all/src/api/hfa_api.dart';
import 'package:headphone_for_all/src/state/navigation.dart';

import 'helpers.dart';

const deskHub = HubInfoDto(
  deviceId: 'd35k-0000-0000-0001',
  name: 'Desk PC',
  addrs: ['192.168.1.20'],
  port: 47810,
  platform: 'windows',
  trusted: true,
);

const macHub = HubInfoDto(
  deviceId: 'mac0-0000-0000-0002',
  name: 'Office Mac',
  addrs: ['192.168.1.31'],
  port: 47810,
  platform: 'macos',
  trusted: false,
);

FakeHfaApi senderFake({String platform = 'linux'}) {
  final fake = FakeHfaApi(
    platform: platform,
    discoverableHubs: [deskHub, macHub],
    trusted: [
      TrustedPeerDto(
        deviceId: deskHub.deviceId,
        name: deskHub.name,
        pairedAtUnix: 1767225600,
      ),
      const TrustedPeerDto(
        deviceId: 'old0-0000-0000-0003',
        name: 'Old laptop',
        pairedAtUnix: 1735689600,
      ),
    ],
    captureApps: const [
      CaptureAppDto(pid: 4242, name: 'Firefox'),
      CaptureAppDto(pid: 5151, name: 'Spotify'),
    ],
  );
  return fake;
}

void main() {
  testWidgets('lists discovered hubs and paired devices not seen', (
    tester,
  ) async {
    final fake = senderFake();
    // A hub announcing this very device is never offered (loop protection).
    fake.discoverableHubs.add(
      HubInfoDto(
        deviceId: fake.appInfo.deviceId,
        name: 'Myself',
        addrs: const ['127.0.0.1'],
        port: 1,
        platform: 'linux',
        trusted: false,
      ),
    );
    await pumpApp(tester, fake, section: AppSection.sender);

    expect(find.text('Desk PC'), findsOneWidget);
    expect(find.text('Office Mac'), findsOneWidget);
    expect(find.text('Myself'), findsNothing);
    expect(find.text('192.168.1.20:47810 · paired'), findsOneWidget);
    expect(find.text('192.168.1.31:47810 · needs PIN'), findsOneWidget);
    expect(find.text('Paired, not seen right now'), findsOneWidget);
    expect(find.text('Old laptop'), findsOneWidget);
    // Desktop: no camera, but pairing links.
    expect(find.byKey(const Key('scan-qr')), findsNothing);
    expect(find.byKey(const Key('pairing-link')), findsOneWidget);

    fake.emitDiscovery(DiscoveryEventDto.lost(deviceId: macHub.deviceId));
    await tester.pumpAndSettle();
    expect(find.text('Office Mac'), findsNothing);
    await unmount(tester);
    expect(fake.calls, contains('stopDiscovery'));
  });

  testWidgets('start and stop on a paired hub', (tester) async {
    final fake = senderFake();
    await pumpApp(tester, fake, section: AppSection.sender);

    final start = find.byKey(const Key('sender-start'));
    expect(tester.widget<FilledButton>(start).onPressed, isNull);

    await tester.tap(find.text('Desk PC'));
    await tester.pumpAndSettle();
    expect(tester.widget<FilledButton>(start).onPressed, isNotNull);

    await tester.tap(start);
    await tester.pumpAndSettle();
    expect(find.text('Streaming'), findsOneWidget);
    expect(find.text('To Desk PC'), findsOneWidget);
    expect(find.text('RTT 5 ms'), findsOneWidget);
    expect(find.text('128 kbit/s'), findsOneWidget);
    expect(find.byKey(const Key('sender-stop')), findsOneWidget);
    final request = fake.lastSenderStart!;
    expect(request.hubDeviceId, deskHub.deviceId);
    expect(request.source, const CaptureSourceDto.system());

    await tester.tap(find.byKey(const Key('sender-stop')));
    await tester.pumpAndSettle();
    expect(find.text('Not sending'), findsOneWidget);
    expect(find.byKey(const Key('sender-start')), findsOneWidget);
    await unmount(tester);
  });

  testWidgets('an unpaired hub asks for the PIN before starting', (
    tester,
  ) async {
    final fake = senderFake();
    await pumpApp(tester, fake, section: AppSection.sender);
    await tester.tap(find.text('Office Mac'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('sender-start')));
    await tester.pumpAndSettle();
    expect(find.text('Enter PIN'), findsWidgets);

    await tester.enterText(find.byKey(const Key('pin-field')), '482913');
    await tester.pumpAndSettle();
    await tester.tap(find.text('Pair'));
    await tester.pumpAndSettle();
    expect(fake.lastSenderStart?.pairingSecret, '482913');
    expect(find.text('Streaming'), findsOneWidget);
    await unmount(tester);
  });

  testWidgets('a pairing-required failure offers "Enter PIN"', (tester) async {
    final fake = senderFake();
    await pumpApp(tester, fake, section: AppSection.sender);
    await tester.tap(find.byKey(const Key('add-by-address')));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('host-field')), '10.0.0.9');
    await tester.enterText(find.byKey(const Key('port-field')), '47999');
    await tester.tap(find.text('Add'));
    await tester.pumpAndSettle();
    expect(find.text('10.0.0.9:47999'), findsWidgets);

    await tester.tap(find.byKey(const Key('sender-start')));
    await tester.pumpAndSettle();
    expect(find.text('Failed'), findsOneWidget);
    expect(find.textContaining('pairing required'), findsOneWidget);
    expect(fake.lastSenderStart?.hubHost, '10.0.0.9');
    expect(fake.lastSenderStart?.hubPort, 47999);

    await tester.tap(find.byKey(const Key('enter-pin')));
    await tester.pumpAndSettle();
    await tester.enterText(find.byKey(const Key('pin-field')), '111222');
    await tester.pumpAndSettle();
    await tester.tap(find.text('Pair'));
    await tester.pumpAndSettle();
    expect(fake.lastSenderStart?.pairingSecret, '111222');
    expect(find.text('Streaming'), findsOneWidget);
    await unmount(tester);
  });

  testWidgets('desktop sources follow the capabilities; one app is picked', (
    tester,
  ) async {
    final fake = senderFake();
    await pumpApp(tester, fake, section: AppSection.sender);
    expect(find.byKey(const Key('source-system')), findsOneWidget);
    expect(find.byKey(const Key('source-systemExceptThisApp')), findsOneWidget);
    expect(find.byKey(const Key('source-app')), findsOneWidget);
    expect(find.byKey(const Key('source-tone')), findsOneWidget);
    expect(find.byKey(const Key('source-deviceAudio')), findsNothing);
    expect(find.byKey(const Key('source-broadcast')), findsNothing);

    await tester.tap(find.text('Desk PC'));
    await tester.tap(find.byKey(const Key('source-app')));
    await tester.pumpAndSettle();
    // No app chosen yet: cannot start.
    final start = find.byKey(const Key('sender-start'));
    expect(tester.widget<FilledButton>(start).onPressed, isNull);

    await tester.tap(find.byKey(const Key('app-dropdown')));
    await tester.pumpAndSettle();
    await tester.tap(find.text('Spotify (5151)').last);
    await tester.pumpAndSettle();
    await tester.tap(start);
    await tester.pumpAndSettle();
    expect(
      fake.lastSenderStart?.source,
      const CaptureSourceDto.process(pid: 5151),
    );
    expect(fake.lastSenderStart?.label, 'Spotify');
    await unmount(tester);
  });

  testWidgets('Android offers native capture only and scans QR codes', (
    tester,
  ) async {
    final fake = senderFake(platform: 'android');
    final native = RecordingNativeChannel();
    await pumpApp(tester, fake, native: native, section: AppSection.sender);
    expect(find.byKey(const Key('source-deviceAudio')), findsOneWidget);
    expect(find.byKey(const Key('source-tone')), findsOneWidget);
    expect(find.byKey(const Key('source-system')), findsNothing);
    expect(find.byKey(const Key('source-app')), findsNothing);
    expect(find.byKey(const Key('scan-qr')), findsOneWidget);
    expect(native.calls, contains('acquireMulticastLock'));

    await tester.tap(find.text('Desk PC'));
    await tester.pumpAndSettle();
    await tester.tap(find.byKey(const Key('sender-start')));
    await tester.pumpAndSettle();
    expect(
      fake.lastSenderStart?.source,
      const CaptureSourceDto.external_(
        feedId: 1,
        sampleRate: 48000,
        channels: 2,
      ),
    );
    expect(native.calls, contains('startSystemCapture'));
    expect(find.text('Streaming'), findsOneWidget);
    await unmount(tester);
    await tester.pump(); // discovery stops asynchronously
    expect(native.calls, contains('releaseMulticastLock'));
  });

  testWidgets('iOS offers the broadcast and prepares the extension', (
    tester,
  ) async {
    final fake = senderFake(platform: 'ios');
    final native = RecordingNativeChannel();
    await pumpApp(tester, fake, native: native, section: AppSection.sender);
    expect(find.byKey(const Key('source-broadcast')), findsOneWidget);
    expect(find.byKey(const Key('source-tone')), findsOneWidget);
    expect(find.textContaining('DRM'), findsWidgets);

    await tester.tap(find.text('Desk PC'));
    await tester.pumpAndSettle();
    expect(find.text('Prepare broadcast'), findsOneWidget);
    await tester.tap(find.byKey(const Key('sender-start')));
    await tester.pumpAndSettle();
    expect(native.lastBroadcastConfig?.hubDeviceId, deskHub.deviceId);
    expect(find.text('Ready to broadcast to Desk PC'), findsOneWidget);
    expect(find.textContaining('Start Broadcast'), findsOneWidget);
    await unmount(tester);
  });

  testWidgets('a pasted pairing link selects the hub with its key', (
    tester,
  ) async {
    final fake = senderFake();
    await pumpApp(tester, fake, section: AppSection.sender);
    await tester.tap(find.byKey(const Key('pairing-link')));
    await tester.pumpAndSettle();
    await tester.enterText(
      find.byKey(const Key('link-field')),
      'hfa://pair?v=0&h=10.1.1.1&p=47810&id=KEYKEY&t=TOKEN&n=Attic',
    );
    await tester.tap(find.text('Use'));
    await tester.pumpAndSettle();
    expect(find.text('Attic'), findsWidgets);
    expect(find.text('10.1.1.1:47810 · from QR code'), findsOneWidget);

    await tester.tap(find.byKey(const Key('sender-start')));
    await tester.pumpAndSettle();
    expect(fake.lastSenderStart?.hubKey, 'KEYKEY');
    expect(fake.lastSenderStart?.pairingSecret, 'TOKEN');
    await unmount(tester);
  });
}
