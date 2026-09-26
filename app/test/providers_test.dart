import 'dart:async';
import 'dart:io';

import 'package:flutter/services.dart' show PlatformException;
import 'package:flutter/widgets.dart' show AppLifecycleState;
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:headphone_for_all/src/api/hfa_api.dart';
import 'package:headphone_for_all/src/models/hub_target.dart';
import 'package:headphone_for_all/src/models/source_choice.dart';
import 'package:headphone_for_all/src/platform/native_channel.dart';
import 'package:headphone_for_all/src/state/app_prefs.dart';
import 'package:headphone_for_all/src/state/core_providers.dart';
import 'package:headphone_for_all/src/state/discovery_controller.dart';
import 'package:headphone_for_all/src/state/hub_address_book.dart';
import 'package:headphone_for_all/src/state/hub_controller.dart';
import 'package:headphone_for_all/src/state/pairing_controller.dart';
import 'package:headphone_for_all/src/state/sender_controller.dart';
import 'package:headphone_for_all/src/state/settings_controller.dart';

import 'helpers.dart';

/// Lets stream events and microtasks run.
Future<void> settle() => Future<void>.delayed(Duration.zero);

/// A live sender's status.
const streamingStatus = SenderStatusDto(
  state: 'streaming',
  hubName: 'Desk PC',
  bitrate: 128000,
  lossPct: 0,
  rttMs: 3,
  levelDb: -20,
  hubGain: 1,
  hubMuted: false,
  hubPriority: false,
);

const trustedHub = HubInfoDto(
  deviceId: 'hub1-0000-0000-0001',
  name: 'Desk PC',
  addrs: ['192.168.1.20', 'fe80::1'],
  port: 47810,
  platform: 'windows',
  trusted: true,
);

/// [trustedHub] in the trust store.
final trustedPeer = TrustedPeerDto(
  deviceId: trustedHub.deviceId,
  name: trustedHub.name,
  pairedAtUnix: 1767225600,
  pairedAsHub: true,
  pairedAsSender: true,
);

const strangerHub = HubInfoDto(
  deviceId: 'hub2-0000-0000-0002',
  name: 'Office Mac',
  addrs: ['192.168.1.31'],
  port: 47811,
  platform: 'macos',
  trusted: false,
);

void main() {
  // The iOS sender listens to app lifecycle changes (AppLifecycleListener).
  TestWidgetsFlutterBinding.ensureInitialized();

  group('HubController', () {
    test('start runs the hub service and loads existing sources', () async {
      final fake = FakeHfaApi()..addSource(FakeHfaApi.source(streamId: 7));
      final native = RecordingNativeChannel();
      final c = containerFor(fake, native: native);
      c.listen(hubControllerProvider, (_, _) {});

      await c.read(hubControllerProvider.notifier).start();
      await settle();

      final hub = c.read(hubControllerProvider);
      expect(hub.running, isTrue);
      expect(hub.port, 47810);
      expect(hub.sources.map((s) => s.streamId), [7]);
      expect(native.calls, contains('startHubService'));
      expect(fake.calls.indexOf('hubStart'), greaterThan(-1));
    });

    test('events add, update and remove sources in order', () async {
      final fake = FakeHfaApi();
      final c = containerFor(fake);
      c.listen(hubControllerProvider, (_, _) {});
      await c.read(hubControllerProvider.notifier).start();

      fake.addSource(FakeHfaApi.source(streamId: 1, deviceName: 'A'));
      fake.addSource(FakeHfaApi.source(streamId: 2, deviceName: 'B'));
      await settle();
      expect(c.read(hubControllerProvider).sources.map((s) => s.deviceName), [
        'A',
        'B',
      ]);

      fake.emitHubEvent(
        HubEventDto.sourceUpdated(
          FakeHfaApi.source(streamId: 1, deviceName: 'A', levelDb: -6),
        ),
      );
      await settle();
      expect(c.read(hubControllerProvider).sources.first.levelDb, -6);

      fake.removeSource(1);
      await settle();
      expect(c.read(hubControllerProvider).sources.map((s) => s.streamId), [2]);
    });

    test('polling picks up sources whose events were missed', () async {
      final fake = FakeHfaApi();
      final c = containerFor(
        fake,
        pollInterval: const Duration(milliseconds: 20),
      );
      c.listen(hubControllerProvider, (_, _) {});
      await c.read(hubControllerProvider.notifier).start();

      fake.addSource(FakeHfaApi.source(streamId: 9), emit: false);
      expect(c.read(hubControllerProvider).sources, isEmpty);
      await Future<void>.delayed(const Duration(milliseconds: 60));
      expect(c.read(hubControllerProvider).sources.single.streamId, 9);
    });

    test(
      'controls are optimistic and reverted when the core refuses',
      () async {
        final fake = FakeHfaApi()..addSource(FakeHfaApi.source(streamId: 3));
        final c = containerFor(fake);
        c.listen(hubControllerProvider, (_, _) {});
        final hub = c.read(hubControllerProvider.notifier);
        await hub.start();

        await hub.setGain(3, 0.5);
        await hub.setMuted(3, true);
        await hub.setPriority(3, true);
        expect(fake.sources[3]!.gain, 0.5);
        expect(fake.sources[3]!.muted, isTrue);
        expect(fake.sources[3]!.priority, isTrue);
        final source = c.read(hubControllerProvider).sources.single;
        expect((source.gain, source.muted, source.priority), (0.5, true, true));

        await hub.setGain(3, 9); // out of range: refused
        final after = c.read(hubControllerProvider);
        expect(after.sources.single.gain, 0.5);
        expect(after.error, contains('gain'));
        hub.clearError();
        expect(c.read(hubControllerProvider).error, isNull);
      },
    );

    test(
      'the master gain is saved by the core and shown after a restart',
      () async {
        final fake = FakeHfaApi();
        var c = containerFor(fake);
        c.listen(hubControllerProvider, (_, _) {});
        var hub = c.read(hubControllerProvider.notifier);
        await settle();
        expect(c.read(hubControllerProvider).masterGain, 1);
        // Saved while the hub is stopped too.
        await hub.setMasterGain(1.5);
        expect(fake.calls, contains('hubSetMasterGain'));
        expect(fake.masterGain, 1.5);
        c.dispose();

        // A new UI (app restart) shows the saved value before any start.
        c = containerFor(fake);
        c.listen(hubControllerProvider, (_, _) {});
        await settle();
        expect(c.read(hubControllerProvider).masterGain, 1.5);
        hub = c.read(hubControllerProvider.notifier);
        await hub.start();
        expect(c.read(hubControllerProvider).masterGain, 1.5);
        await hub.setMasterGain(0.25);
        expect(fake.masterGain, 0.25);
        await hub.stop();
        expect(c.read(hubControllerProvider).masterGain, 0.25);
      },
    );

    test('stop clears sources and stops the hub service', () async {
      final fake = FakeHfaApi()..addSource(FakeHfaApi.source(streamId: 3));
      final native = RecordingNativeChannel();
      final c = containerFor(fake, native: native);
      c.listen(hubControllerProvider, (_, _) {});
      final hub = c.read(hubControllerProvider.notifier);
      await hub.start();
      await hub.stop();
      final state = c.read(hubControllerProvider);
      expect(state.running, isFalse);
      expect(state.sources, isEmpty);
      expect(fake.hubRunning, isFalse);
      expect(
        native.calls,
        containsAllInOrder(['startHubService', 'stopHubService']),
      );
    });

    test('a failed start reports the error and releases the service', () async {
      final fake = _FailingHubApi();
      final native = RecordingNativeChannel();
      final c = containerFor(fake, native: native);
      c.listen(hubControllerProvider, (_, _) {});
      await c.read(hubControllerProvider.notifier).start();
      final state = c.read(hubControllerProvider);
      expect(state.running, isFalse);
      expect(state.busy, isFalse);
      expect(state.error, 'no output device');
      expect(native.calls, ['startHubService', 'stopHubService']);
    });

    test('a refused master gain goes back to the saved value', () async {
      final fake = _MasterGainFailingApi();
      final native = RecordingNativeChannel();
      final c = containerFor(fake, native: native);
      // Errors are transient (the hub screen shows each in a snack bar).
      final errors = <String>[];
      c.listen(hubControllerProvider.select((h) => h.error), (_, error) {
        if (error != null) errors.add(error);
      });
      final hub = c.read(hubControllerProvider.notifier);
      await hub.setMasterGain(0.5);
      await hub.start();
      var state = c.read(hubControllerProvider);
      expect(state.running, isTrue);
      expect(state.masterGain, 0.5, reason: 'the core applies it on start');
      await hub.setMasterGain(2);
      state = c.read(hubControllerProvider);
      expect(fake.hubRunning, isTrue);
      expect(errors, ['mixer busy']);
      expect(state.masterGain, 0.5);
      expect(native.calls, ['startHubService']);
    });

    test('the header data follows the core status', () async {
      final fake = FakeHfaApi()..advertiseError = 'no multicast';
      final c = containerFor(
        fake,
        pollInterval: const Duration(milliseconds: 20),
      );
      c.listen(hubControllerProvider, (_, _) {});
      final hub = c.read(hubControllerProvider.notifier);
      await hub.start();
      var state = c.read(hubControllerProvider);
      expect(state.addresses, ['192.168.1.10:47810', '[fd00::10]:47810']);
      expect(state.notDiscoverable, isTrue);
      expect(state.advertiseError, 'no multicast');
      // The poll picks up a new network.
      fake
        ..lanAddresses = ['10.0.0.5']
        ..advertiseError = null;
      await Future<void>.delayed(const Duration(milliseconds: 80));
      state = c.read(hubControllerProvider);
      expect(state.addresses, ['10.0.0.5:47810']);
      expect(state.notDiscoverable, isFalse);
      await hub.stop();
      state = c.read(hubControllerProvider);
      expect(state.addresses, isEmpty);
      expect(state.notDiscoverable, isFalse);
    });

    test('an Error event is surfaced', () async {
      final fake = FakeHfaApi();
      final c = containerFor(fake);
      c.listen(hubControllerProvider, (_, _) {});
      fake.emitHubEvent(const HubEventDto.error(message: 'output lost'));
      await settle();
      expect(c.read(hubControllerProvider).error, 'output lost');
    });
  });

  group('PairingController', () {
    test(
      'opens a window, keeps it after a failed attempt, completes',
      () async {
        final fake = FakeHfaApi();
        final c = containerFor(fake);
        c.listen(hubControllerProvider, (_, _) {});
        c.listen(pairingControllerProvider, (_, _) {});
        await c.read(hubControllerProvider.notifier).start();
        await c.read(pairingControllerProvider.notifier).start();

        var state = c.read(pairingControllerProvider);
        expect(state.phase, PairingPhase.waiting);
        expect(state.info?.pin, '482913');

        fake.emitHubEvent(const HubEventDto.pairingFailed(reason: 'wrong PIN'));
        await settle();
        state = c.read(pairingControllerProvider);
        expect(state.phase, PairingPhase.waiting);
        expect(state.message, 'wrong PIN');
        expect(state.info?.pin, '482913');

        fake.completePairing(deviceId: 'p1', name: 'Pixel');
        await settle();
        state = c.read(pairingControllerProvider);
        expect(state.phase, PairingPhase.completed);
        expect(state.pairedName, 'Pixel');
      },
    );

    test(
      'shows that the core closed the window after failed attempts',
      () async {
        final fake = FakeHfaApi();
        final c = containerFor(fake);
        c.listen(hubControllerProvider, (_, _) {});
        c.listen(pairingControllerProvider, (_, _) {});
        await c.read(hubControllerProvider.notifier).start();
        await c.read(pairingControllerProvider.notifier).start();

        // The fifth wrong PIN: the core closes the window without an event.
        fake.closePairingWindow();
        fake.emitHubEvent(const HubEventDto.pairingFailed(reason: 'wrong PIN'));
        await settle();
        final state = c.read(pairingControllerProvider);
        expect(state.phase, PairingPhase.failed);
        expect(state.message, contains('closed this pairing window'));
        expect(fake.calls, contains('hubPairingStatus'));
      },
    );

    test('fails to open while the hub is stopped', () async {
      final c = containerFor(FakeHfaApi());
      c.listen(pairingControllerProvider, (_, _) {});
      await c.read(pairingControllerProvider.notifier).start();
      final state = c.read(pairingControllerProvider);
      expect(state.phase, PairingPhase.failed);
      expect(state.message, 'the hub is not running');
    });

    test('cancel closes an open window in the core', () async {
      final fake = FakeHfaApi();
      final c = containerFor(fake);
      c.listen(pairingControllerProvider, (_, _) {});
      await fake.hubStart();
      await c.read(pairingControllerProvider.notifier).start();
      await c.read(pairingControllerProvider.notifier).cancel();
      expect(fake.calls, contains('hubCancelPairing'));
      expect(fake.pairing, isNull);
      expect(c.read(pairingControllerProvider).phase, PairingPhase.idle);
    });

    test('a cancel while opening closes the late window in the core', () async {
      final fake = _SlowPairingApi();
      final c = containerFor(fake);
      c.listen(pairingControllerProvider, (_, _) {});
      await fake.hubStart();
      final pairing = c.read(pairingControllerProvider.notifier);
      final opening = pairing.start();
      await settle();
      expect(c.read(pairingControllerProvider).phase, PairingPhase.opening);
      await pairing.cancel();
      // The core opens the window only after the cancel ran.
      fake.open.complete();
      await opening;
      expect(c.read(pairingControllerProvider).phase, PairingPhase.idle);
      expect(fake.calls.where((m) => m == 'hubCancelPairing'), hasLength(2));
      expect(fake.pairing, isNull);
    });

    test('a newer start keeps its window when an older one resolves', () async {
      final fake = _SlowPairingApi();
      final c = containerFor(fake);
      c.listen(pairingControllerProvider, (_, _) {});
      await fake.hubStart();
      final pairing = c.read(pairingControllerProvider.notifier);
      final first = pairing.start();
      await settle();
      final firstOpen = fake.open;
      fake.open = Completer<void>();
      final second = pairing.start();
      await settle();
      firstOpen.complete();
      await first;
      fake.open.complete();
      await second;
      expect(c.read(pairingControllerProvider).phase, PairingPhase.waiting);
      expect(fake.calls, isNot(contains('hubCancelPairing')));
    });

    test('secondsLeft counts down and never goes negative', () {
      const state = PairingState(
        phase: PairingPhase.waiting,
        info: PairingInfoDto(
          pin: '000000',
          token: 't',
          uri: 'hfa://pair',
          expiresAtUnix: 1000,
        ),
      );
      DateTime at(int s) => DateTime.fromMillisecondsSinceEpoch(s * 1000);
      expect(state.secondsLeft(at(700)), 300);
      expect(state.secondsLeft(at(999)), 1);
      expect(state.secondsLeft(at(2000)), 0);
    });
  });

  group('SenderController', () {
    test('mirrors the status stream and starts on a trusted hub', () async {
      final fake = FakeHfaApi(trusted: [trustedPeer]);
      final c = containerFor(fake);
      c.listen(senderControllerProvider, (_, _) {});
      final sender = c.read(senderControllerProvider.notifier);
      sender.selectTarget(HubTarget.discovered(trustedHub));
      sender.selectSource(const SourceChoice(SourceKind.tone));
      await sender.start();
      await settle();

      final state = c.read(senderControllerProvider);
      expect(state.status.state, 'streaming');
      expect(state.isLive, isTrue);
      final request = fake.lastSenderStart!;
      expect(request.hubHost, '192.168.1.20');
      expect(request.hubPort, 47810);
      expect(request.hubDeviceId, trustedHub.deviceId);
      expect(request.pairingSecret, isNull);
      expect(request.source, const CaptureSourceDto.tone(freqHz: 440));

      await sender.stop();
      await settle();
      expect(c.read(senderControllerProvider).status.state, 'idle');
    });

    test(
      'a pairing failure asks for a PIN; the PIN is sent next time',
      () async {
        final fake = FakeHfaApi(trusted: [trustedPeer]);
        final c = containerFor(fake);
        c.listen(senderControllerProvider, (_, _) {});
        final sender = c.read(senderControllerProvider.notifier);
        sender.selectTarget(HubTarget.discovered(strangerHub));
        await sender.start();
        await settle();
        var state = c.read(senderControllerProvider);
        expect(state.status.state, 'failed');
        expect(state.needsPin, isTrue);

        await sender.start(pin: ' 123456 ');
        await settle();
        state = c.read(senderControllerProvider);
        expect(state.status.state, 'streaming');
        expect(fake.lastSenderStart!.pairingSecret, '123456');
        // Paired now: the one-time PIN is dropped and the hub is trusted.
        expect(state.target?.pairingSecret, isNull);
        expect(state.target?.trusted, isTrue);
        expect(
          fake.trusted.map((p) => p.deviceId),
          contains(strangerHub.deviceId),
        );
      },
    );

    test('a refused start shows the core error', () async {
      final fake = FakeHfaApi(trusted: [trustedPeer])
        ..emitSenderStatus(
          const SenderStatusDto(
            state: 'streaming',
            bitrate: 1,
            lossPct: 0,
            rttMs: 0,
            levelDb: 0,
            hubGain: 1,
            hubMuted: false,
            hubPriority: false,
          ),
        );
      final c = containerFor(fake);
      c.listen(senderControllerProvider, (_, _) {});
      final sender = c.read(senderControllerProvider.notifier);
      sender.selectTarget(HubTarget.discovered(trustedHub));
      // The controller thinks it is idle until the stream catches up, so it
      // calls the core, which refuses.
      await sender.start();
      expect(
        c.read(senderControllerProvider).error,
        'a sender is already running',
      );
    });

    test('Android: external feed first, then the native capture', () async {
      final fake = FakeHfaApi(platform: 'android', trusted: [trustedPeer]);
      final native = RecordingNativeChannel();
      final c = containerFor(fake, native: native);
      c.listen(senderControllerProvider, (_, _) {});
      final sender = c.read(senderControllerProvider.notifier);
      expect(
        c.read(senderControllerProvider).source?.kind,
        SourceKind.deviceAudio,
      );
      sender.selectTarget(HubTarget.discovered(trustedHub));
      await sender.start();

      expect(
        fake.lastSenderStart!.source,
        const CaptureSourceDto.external_(
          feedId: 1,
          sampleRate: 48000,
          channels: 2,
        ),
      );
      expect(native.lastCaptureArgs, {
        'feedId': 1,
        'sampleRate': 48000,
        'channels': 2,
      });
      expect(fake.calls.last, 'senderStart');

      await sender.stop();
      expect(
        native.calls,
        containsAllInOrder(['startSystemCapture', 'stopSystemCapture']),
      );
      expect(fake.calls.last, 'senderStop');
    });

    test('Android: denied capture stops the sender and explains', () async {
      final fake = FakeHfaApi(platform: 'android', trusted: [trustedPeer]);
      final native = RecordingNativeChannel(captureAllowed: false);
      final c = containerFor(fake, native: native);
      c.listen(senderControllerProvider, (_, _) {});
      final sender = c.read(senderControllerProvider.notifier);
      sender.selectTarget(HubTarget.discovered(trustedHub));
      await sender.start();
      await settle();
      final state = c.read(senderControllerProvider);
      expect(fake.calls, containsAllInOrder(['senderStart', 'senderStop']));
      expect(state.status.state, 'idle');
      expect(state.error, contains('not allowed'));
    });

    test(
      'Android: a sender that fails during the consent dialog stops capture',
      () async {
        // A hub typed in by address without a PIN: the core fails with
        // "pairing required" while the consent dialog is still open.
        final fake = FakeHfaApi(platform: 'android');
        final native = _ConsentNativeChannel();
        final c = containerFor(fake, native: native);
        c.listen(senderControllerProvider, (_, _) {});
        final sender = c.read(senderControllerProvider.notifier);
        sender.selectTarget(HubTarget.manual(host: '10.0.0.9'));
        final starting = sender.start();
        await settle();
        // The failure stops the capture at once, which cancels the start.
        expect(native.calls, ['startSystemCapture', 'stopSystemCapture']);
        expect(fake.senderStatusNow.state, 'failed');
        // A cancelled start answers false (§8.8).
        native.consent.complete(false);
        await starting;
        await settle();

        final state = c.read(senderControllerProvider);
        expect(native.calls, ['startSystemCapture', 'stopSystemCapture']);
        expect(state.isLive, isFalse);
        expect(state.busy, isFalse);
        expect(state.error, contains('pairing required'));
        expect(state.needsPin, isTrue);
      },
    );

    test('Android: a captureStopped event stops the sender', () async {
      final fake = FakeHfaApi(platform: 'android', trusted: [trustedPeer]);
      final native = RecordingNativeChannel();
      final c = containerFor(fake, native: native);
      c.listen(senderControllerProvider, (_, _) {});
      final sender = c.read(senderControllerProvider.notifier);
      sender.selectTarget(HubTarget.discovered(trustedHub));
      await sender.start();
      native.emit(
        const NativeEvent(NativeEventType.captureStopped, message: 'revoked'),
      );
      await settle();
      await settle();
      final state = c.read(senderControllerProvider);
      expect(state.status.state, 'idle');
      expect(state.error, 'Capture stopped: revoked');
    });

    group('Android: a new UI while the process lives on', () {
      /// Starts device-audio streaming in a first UI, then drops that UI
      /// (the activity was destroyed; the engine and the service live on).
      Future<void> streamThenCloseUi(
        FakeHfaApi fake,
        RecordingNativeChannel native,
      ) async {
        final first = containerFor(fake, native: native);
        first.listen(senderControllerProvider, (_, _) {});
        final sender = first.read(senderControllerProvider.notifier);
        sender.selectTarget(HubTarget.discovered(trustedHub));
        await sender.start();
        await settle();
        expect(first.read(senderControllerProvider).isLive, isTrue);
        first.dispose();
        native.calls.clear();
        fake.calls.clear();
      }

      test('adopts the running capture', () async {
        final fake = FakeHfaApi(platform: 'android', trusted: [trustedPeer]);
        final native = RecordingNativeChannel();
        await streamThenCloseUi(fake, native);
        native.captureStatusAnswer = const NativeCaptureStatus(running: true);

        final c = containerFor(fake, native: native);
        c.listen(senderControllerProvider, (_, _) {});
        await settle();
        await settle();
        expect(c.read(senderControllerProvider).isLive, isTrue);
        // Adopted: a capture end reaches this UI and stops the sender.
        native.emit(
          const NativeEvent(NativeEventType.captureStopped, message: 'gone'),
        );
        await settle();
        await settle();
        expect(fake.calls, contains('senderStop'));
        expect(c.read(senderControllerProvider).error, 'Capture stopped: gone');
      });

      test('Stop ends a capture this UI did not start', () async {
        final fake = FakeHfaApi(platform: 'android', trusted: [trustedPeer]);
        final native = RecordingNativeChannel();
        await streamThenCloseUi(fake, native);
        // captureStatus says nothing useful (for example not answered yet).
        final c = containerFor(fake, native: native);
        c.listen(senderControllerProvider, (_, _) {});
        await settle();
        await c.read(senderControllerProvider.notifier).stop();
        expect(native.calls, ['stopSystemCapture']);
        expect(fake.calls.last, 'senderStop');
      });

      test('stops a sender whose capture ended unheard', () async {
        final fake = FakeHfaApi(platform: 'android', trusted: [trustedPeer]);
        final native = RecordingNativeChannel();
        await streamThenCloseUi(fake, native);
        native.captureStatusAnswer = const NativeCaptureStatus(
          running: false,
          endedWhileAway: 'screen locked',
        );

        final c = containerFor(fake, native: native);
        c.listen(senderControllerProvider, (_, _) {});
        await settle();
        await settle();
        await settle();
        expect(fake.calls, contains('senderStop'));
        final state = c.read(senderControllerProvider);
        expect(state.isLive, isFalse);
        expect(state.error, contains('while the app was closed'));
        expect(state.error, contains('screen locked'));
      });

      test('stops a capture whose sender ended', () async {
        final fake = FakeHfaApi(platform: 'android', trusted: [trustedPeer]);
        final native = RecordingNativeChannel();
        await streamThenCloseUi(fake, native);
        await fake.senderStop();
        native.captureStatusAnswer = const NativeCaptureStatus(running: true);

        final c = containerFor(fake, native: native);
        c.listen(senderControllerProvider, (_, _) {});
        await settle();
        await settle();
        expect(native.calls, ['stopSystemCapture']);
      });
    });

    test('Android: a refused recording permission stops the sender', () async {
      final fake = FakeHfaApi(platform: 'android', trusted: [trustedPeer]);
      final native = _PermissionDeniedNativeChannel();
      final c = containerFor(fake, native: native);
      c.listen(senderControllerProvider, (_, _) {});
      final sender = c.read(senderControllerProvider.notifier);
      sender.selectTarget(HubTarget.discovered(trustedHub));
      await sender.start();
      await settle();
      final state = c.read(senderControllerProvider);
      expect(fake.calls, containsAllInOrder(['senderStart', 'senderStop']));
      expect(state.isLive, isFalse);
      expect(state.busy, isFalse);
      expect(state.error, contains('audio recording permission'));
    });

    test(
      'Android: a sender adopted from an earlier UI stops its capture',
      () async {
        // The Flutter engine was recreated while the core kept sending.
        final fake = FakeHfaApi(platform: 'android', trusted: [trustedPeer]);
        fake.emitSenderStatus(streamingStatus);
        final native = RecordingNativeChannel();
        final c = containerFor(fake, native: native);
        c.listen(senderControllerProvider, (_, _) {});
        await settle();
        expect(c.read(senderControllerProvider).isLive, isTrue);
        expect(native.calls, isEmpty);

        await c.read(senderControllerProvider.notifier).stop();
        expect(native.calls, ['stopSystemCapture']);
        expect(fake.calls.last, 'senderStop');
      },
    );

    test('Android: an adopted sender honours capture events', () async {
      final fake = FakeHfaApi(platform: 'android', trusted: [trustedPeer]);
      fake.emitSenderStatus(streamingStatus);
      final native = RecordingNativeChannel();
      final c = containerFor(fake, native: native);
      c.listen(senderControllerProvider, (_, _) {});
      await settle();
      native.emit(
        const NativeEvent(NativeEventType.captureStopped, message: 'revoked'),
      );
      await settle();
      await settle();
      final state = c.read(senderControllerProvider);
      expect(fake.calls, contains('senderStop'));
      expect(state.status.state, 'idle');
      expect(state.error, 'Capture stopped: revoked');
    });

    test('Android: an adopted sender that fails stops its capture', () async {
      final fake = FakeHfaApi(platform: 'android', trusted: [trustedPeer]);
      fake.emitSenderStatus(streamingStatus);
      final native = RecordingNativeChannel();
      final c = containerFor(fake, native: native);
      c.listen(senderControllerProvider, (_, _) {});
      await settle();
      fake.emitSenderStatus(
        const SenderStatusDto(
          state: 'failed',
          error: 'the hub went away',
          bitrate: 0,
          lossPct: 0,
          rttMs: 0,
          levelDb: -120,
          hubGain: 1,
          hubMuted: false,
          hubPriority: false,
        ),
      );
      await settle();
      expect(native.calls, ['stopSystemCapture']);
    });

    test(
      'Android: a capture left behind without a sender is stopped at startup',
      () async {
        final fake = FakeHfaApi(platform: 'android');
        final native = RecordingNativeChannel();
        final c = containerFor(fake, native: native);
        c.listen(senderControllerProvider, (_, _) {});
        await settle();
        expect(native.calls, ['stopSystemCapture']);
      },
    );

    test('Android: the reason of a failed capture start is shown', () async {
      final fake = FakeHfaApi(platform: 'android', trusted: [trustedPeer]);
      final native = _ConsentNativeChannel();
      final c = containerFor(fake, native: native);
      c.listen(senderControllerProvider, (_, _) {});
      final sender = c.read(senderControllerProvider.notifier);
      sender.selectTarget(HubTarget.discovered(trustedHub));
      final starting = sender.start();
      await settle();
      native.emit(
        const NativeEvent(
          NativeEventType.captureError,
          message: 'ForegroundServiceStartNotAllowedException',
        ),
      );
      await settle();
      native.consent.complete(false);
      await starting;
      await settle();
      final state = c.read(senderControllerProvider);
      expect(
        state.error,
        'Audio capture could not start: '
        'ForegroundServiceStartNotAllowedException',
      );
      expect(fake.calls, contains('senderStop'));
      expect(state.isLive, isFalse);
    });

    test(
      'Android: a sender that finds its hub by id holds the multicast lock',
      () async {
        final fake = FakeHfaApi(platform: 'android', trusted: [trustedPeer]);
        final native = RecordingNativeChannel();
        final c = containerFor(fake, native: native);
        c.listen(senderControllerProvider, (_, _) {});
        final sender = c.read(senderControllerProvider.notifier);
        sender.selectTarget(HubTarget.paired(trustedPeer));
        await sender.start();
        await settle();
        expect(c.read(senderControllerProvider).isLive, isTrue);
        expect(native.calls, contains('acquireMulticastLock'));
        expect(native.calls, isNot(contains('releaseMulticastLock')));

        await sender.stop();
        await settle();
        expect(
          native.calls.where((m) => m == 'releaseMulticastLock'),
          hasLength(1),
        );
      },
    );

    test(
      'iOS: pairs through the app, then writes the broadcast config',
      () async {
        final fake = FakeHfaApi(platform: 'ios');
        final native = RecordingNativeChannel();
        final c = containerFor(fake, native: native);
        c.listen(senderControllerProvider, (_, _) {});
        final sender = c.read(senderControllerProvider.notifier);
        expect(
          c.read(senderControllerProvider).source?.kind,
          SourceKind.broadcast,
        );
        sender.selectTarget(HubTarget.discovered(strangerHub));
        await sender.start(pin: '654321');
        await settle();

        expect(fake.calls, containsAllInOrder(['senderStart', 'senderStop']));
        expect(fake.lastSenderStart!.pairingSecret, '654321');
        final config = native.lastBroadcastConfig!;
        expect(config.hubHost, '192.168.1.31');
        expect(config.hubPort, 47811);
        expect(config.hubDeviceId, strangerHub.deviceId);
        final state = c.read(senderControllerProvider);
        expect(state.broadcastReady, isTrue);
        expect(state.status.state, 'idle');

        native.emit(const NativeEvent(NativeEventType.broadcastStarted));
        await settle();
        expect(c.read(senderControllerProvider).broadcasting, isTrue);
        native.emit(const NativeEvent(NativeEventType.broadcastFinished));
        await settle();
        expect(c.read(senderControllerProvider).broadcasting, isFalse);
      },
    );

    test('pairing with a PIN refreshes the trusted devices', () async {
      final fake = FakeHfaApi();
      final c = containerFor(fake);
      c.listen(senderControllerProvider, (_, _) {});
      c.listen(trustedPeersProvider, (_, _) {});
      expect(await c.read(trustedPeersProvider.future), isEmpty);
      final sender = c.read(senderControllerProvider.notifier);
      sender.selectTarget(HubTarget.discovered(strangerHub));
      await sender.start(pin: '123456');
      await settle();
      expect(c.read(senderControllerProvider).target?.trusted, isTrue);
      final peers = await c.read(trustedPeersProvider.future);
      expect(peers.map((p) => p.deviceId), [strangerHub.deviceId]);
    });

    test('iOS: pairing is seen even when status events are missed', () async {
      final fake = _SilentSenderEventsApi(platform: 'ios');
      final native = RecordingNativeChannel();
      final c = containerFor(fake, native: native);
      c.listen(senderControllerProvider, (_, _) {});
      final sender = c.read(senderControllerProvider.notifier);
      sender.selectTarget(HubTarget.discovered(strangerHub));
      // Real timers (the status poll) cannot run inside the fake async zone
      // of `test`, so wait for the outcome with a real bound.
      await sender.start(pin: '654321').timeout(const Duration(seconds: 5));
      final state = c.read(senderControllerProvider);
      expect(state.error, isNull);
      expect(state.broadcastReady, isTrue);
      expect(native.lastBroadcastConfig?.hubDeviceId, strangerHub.deviceId);
    });

    test('iOS: a hub typed by address must be paired first', () async {
      final fake = FakeHfaApi(platform: 'ios');
      final native = RecordingNativeChannel();
      final c = containerFor(fake, native: native);
      c.listen(senderControllerProvider, (_, _) {});
      final sender = c.read(senderControllerProvider.notifier);
      sender.selectTarget(HubTarget.manual(host: '10.0.0.9'));
      await sender.start();
      var state = c.read(senderControllerProvider);
      expect(state.error, contains('Pair with this hub first'));
      expect(state.needsPin, isTrue);
      expect(native.lastBroadcastConfig, isNull);

      // With the PIN it pairs and learns the hub's id from the trust store.
      await sender.start(pin: '222333');
      state = c.read(senderControllerProvider);
      expect(state.error, isNull);
      expect(native.lastBroadcastConfig?.hubHost, '10.0.0.9');
      expect(native.lastBroadcastConfig?.hubDeviceId, 'hub-10.0.0.9');
      expect(state.target?.deviceId, 'hub-10.0.0.9');
      expect(state.target?.trusted, isTrue);
    });

    test('iOS: a paired hub without an address is refused', () async {
      final fake = FakeHfaApi(platform: 'ios', trusted: [trustedPeer]);
      final native = RecordingNativeChannel();
      final c = containerFor(fake, native: native);
      c.listen(senderControllerProvider, (_, _) {});
      final sender = c.read(senderControllerProvider.notifier);
      sender.selectTarget(HubTarget.paired(trustedPeer));
      await sender.start();
      final state = c.read(senderControllerProvider);
      expect(state.error, iosNeedsHubAddress);
      expect(state.busy, isFalse);
      expect(native.lastBroadcastConfig, isNull);
      expect(fake.calls, isNot(contains('senderStart')));
    });

    test(
      'iOS: the address a hub was reached at is remembered across restarts',
      () async {
        final dir = await Directory.systemTemp.createTemp('hfa_book_');
        addTearDown(() => dir.delete(recursive: true));
        ProviderContainer containerIn(FakeHfaApi fake, NativeChannel native) {
          return ProviderContainer.test(
            overrides: [
              ...overridesFor(fake, native: native),
              dataDirProvider.overrideWithValue(dir.path),
            ],
            retry: (retryCount, error) => null,
          );
        }

        // Paired with a PIN at a typed-in address.
        final fake = FakeHfaApi(platform: 'ios');
        var c = containerIn(fake, RecordingNativeChannel());
        c.listen(senderControllerProvider, (_, _) {});
        c.listen(hubAddressBookProvider, (_, _) {});
        final sender = c.read(senderControllerProvider.notifier);
        sender.selectTarget(HubTarget.manual(host: '10.0.0.9', port: 47811));
        await sender.start(pin: '222333');
        expect(c.read(senderControllerProvider).error, isNull);
        expect(
          c.read(hubAddressBookProvider)['hub-10.0.0.9'],
          const HubAddress('10.0.0.9', 47811),
        );
        // Let the write finish, then "restart the app".
        await c.read(hubAddressBookProvider.notifier).flush();
        c.dispose();

        final native = RecordingNativeChannel();
        c = containerIn(fake, native);
        c.listen(senderControllerProvider, (_, _) {});
        c.listen(hubAddressBookProvider, (_, _) {});
        await c.read(hubAddressBookProvider.notifier).flush();
        final book = c.read(hubAddressBookProvider);
        expect(book['hub-10.0.0.9'], const HubAddress('10.0.0.9', 47811));
        final peer = fake.trusted.single;
        c
            .read(senderControllerProvider.notifier)
            .selectTarget(HubTarget.paired(peer, address: book[peer.deviceId]));
        await c.read(senderControllerProvider.notifier).start();
        expect(c.read(senderControllerProvider).error, isNull);
        expect(native.lastBroadcastConfig?.hubHost, '10.0.0.9');
        expect(native.lastBroadcastConfig?.hubPort, 47811);
        expect(native.lastBroadcastConfig?.hubDeviceId, 'hub-10.0.0.9');
        c.dispose();
      },
    );

    test('iOS: a failed pairing does not write the config', () async {
      final fake = FakeHfaApi(platform: 'ios', connectAutomatically: false);
      final native = RecordingNativeChannel();
      final c = containerFor(fake, native: native);
      c.listen(senderControllerProvider, (_, _) {});
      final sender = c.read(senderControllerProvider.notifier);
      sender.selectTarget(HubTarget.discovered(strangerHub));
      final started = sender.start(pin: '111111');
      await settle();
      fake.emitSenderStatus(
        const SenderStatusDto(
          state: 'failed',
          error: 'pairing failed: wrong PIN',
          bitrate: 0,
          lossPct: 0,
          rttMs: 0,
          levelDb: -120,
          hubGain: 1,
          hubMuted: false,
          hubPriority: false,
        ),
      );
      await started;
      expect(native.lastBroadcastConfig, isNull);
      expect(
        c.read(senderControllerProvider).error,
        'pairing failed: wrong PIN',
      );
    });
  });

  group('SenderController (trust, broadcast state, memory)', () {
    const hubKey = 'AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE';
    final keyId = FakeHfaApi.fingerprintFor(hubKey);

    /// What a scanned QR code leaves after its pairing: the key is kept, the
    /// one-time token spent, and the target says "trusted".
    HubTarget scannedThenPaired() => HubTarget.fromPairingUri(
      PairingUriDto(
        host: '10.0.0.9',
        port: 47810,
        hubId: hubKey,
        hubDeviceId: keyId,
        token: 'dG9rZW4',
        name: 'Desk PC',
      ),
    ).asPaired();

    test('iOS: a hub key without a pairing never starts a broadcast', () async {
      final fake = FakeHfaApi(platform: 'ios');
      final native = RecordingNativeChannel();
      final c = containerFor(fake, native: native);
      c.listen(senderControllerProvider, (_, _) {});
      final sender = c.read(senderControllerProvider.notifier);
      final target = scannedThenPaired();
      expect(target.hubKey, hubKey);
      expect(target.pairingSecret, isNull);
      expect(target.trusted, isTrue, reason: 'the stale flag to ignore');

      Future<void> expectRefused(HubTarget target) async {
        sender.selectTarget(target);
        await sender.start();
        final state = c.read(senderControllerProvider);
        expect(state.error, contains('Pair with this hub first'));
        expect(state.needsPin, isTrue);
        expect(state.broadcastReady, isFalse);
        expect(native.lastBroadcastConfig, isNull);
        expect(fake.calls, isNot(contains('senderStart')));
      }

      // Not in the trust store (e.g. forgotten since the QR code).
      await expectRefused(target);
      // Only paired the other way (it sends to this device's hub).
      fake.trusted.add(
        FakeHfaApi.peer(deviceId: keyId, asHub: false, asSender: true),
      );
      await expectRefused(target);
      // A key without a device id is looked up by its fingerprint too.
      await expectRefused(
        const HubTarget(
          name: 'Desk PC',
          origin: HubOrigin.pairingLink,
          host: '10.0.0.9',
          hubKey: hubKey,
          trusted: true,
        ),
      );
      // A key that is not the device id's identifies nothing.
      fake.trusted
        ..clear()
        ..add(FakeHfaApi.peer(deviceId: 'other-id'));
      await expectRefused(
        const HubTarget(
          name: 'Desk PC',
          origin: HubOrigin.pairingLink,
          host: '10.0.0.9',
          deviceId: 'other-id',
          hubKey: hubKey,
          trusted: true,
        ),
      );

      // Paired as a hub: the key's device is found, no PIN needed.
      fake.trusted
        ..clear()
        ..add(FakeHfaApi.peer(deviceId: keyId));
      sender.selectTarget(target);
      await sender.start();
      final state = c.read(senderControllerProvider);
      expect(state.error, isNull);
      expect(state.broadcastReady, isTrue);
      expect(native.lastBroadcastConfig?.hubKey, hubKey);
      expect(fake.calls, isNot(contains('senderStart')));
    });

    test('desktop: a pinned hub key alone does not stream', () async {
      final fake = FakeHfaApi();
      final c = containerFor(fake);
      c.listen(senderControllerProvider, (_, _) {});
      final sender = c.read(senderControllerProvider.notifier);
      sender.selectTarget(scannedThenPaired());
      sender.selectSource(const SourceChoice(SourceKind.tone));
      await sender.start();
      await settle();
      final state = c.read(senderControllerProvider);
      expect(state.status.state, 'failed');
      expect(state.needsPin, isTrue);
      expect(fake.trusted, isEmpty);
    });

    test(
      'iOS: the broadcast state is read at start, on resume and from events',
      () async {
        final fake = FakeHfaApi(platform: 'ios');
        final native = RecordingNativeChannel()
          ..broadcastStatusAnswer = const BroadcastStatus(
            state: BroadcastState.streaming,
            hubName: 'Desk PC',
          );
        final c = containerFor(fake, native: native);
        c.listen(senderControllerProvider, (_, _) {});
        await settle();
        await settle();
        var state = c.read(senderControllerProvider);
        expect(native.broadcastStatusQueries, 1);
        expect(state.broadcast.state, BroadcastState.streaming);
        expect(state.broadcasting, isTrue);

        // Stopped from Control Center while the app was in the background.
        native.broadcastStatusAnswer = const BroadcastStatus(
          state: BroadcastState.stopped,
        );
        final binding = TestWidgetsFlutterBinding.instance;
        binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
        binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
        await settle();
        await settle();
        state = c.read(senderControllerProvider);
        expect(native.broadcastStatusQueries, 2);
        expect(state.broadcast.state, BroadcastState.stopped);
        expect(state.broadcasting, isFalse);

        native.emit(
          const NativeEvent(
            NativeEventType.broadcastStatus,
            broadcast: BroadcastStatus(
              state: BroadcastState.reconnecting,
              hubName: 'Desk PC',
            ),
          ),
        );
        await settle();
        state = c.read(senderControllerProvider);
        expect(state.broadcast.state, BroadcastState.reconnecting);
        expect(state.broadcasting, isTrue);

        native.emit(
          const NativeEvent(
            NativeEventType.broadcastStatus,
            message: 'The hub refused the connection.',
            broadcast: BroadcastStatus(
              state: BroadcastState.failed,
              message: 'The hub refused the connection.',
            ),
          ),
        );
        await settle();
        state = c.read(senderControllerProvider);
        expect(state.broadcasting, isFalse);
        expect(state.error, 'The hub refused the connection.');
      },
    );

    test('other platforms never ask for a broadcast state', () async {
      final native = RecordingNativeChannel();
      final c = containerFor(FakeHfaApi(platform: 'android'), native: native);
      c.listen(senderControllerProvider, (_, _) {});
      await settle();
      expect(native.broadcastStatusQueries, 0);
      expect(c.read(senderControllerProvider).broadcast, BroadcastStatus.idle);
    });

    test('macOS: a sender holds the App Nap opt-out while it runs', () async {
      final fake = FakeHfaApi(platform: 'macos', trusted: [trustedPeer]);
      final native = RecordingNativeChannel();
      final c = containerFor(fake, native: native);
      c.listen(senderControllerProvider, (_, _) {});
      final sender = c.read(senderControllerProvider.notifier);
      sender.selectTarget(HubTarget.discovered(trustedHub));
      sender.selectSource(const SourceChoice(SourceKind.tone));
      await sender.start();
      await settle();
      expect(native.calls, ['beginStreaming']);
      await sender.stop();
      expect(native.calls, ['beginStreaming', 'endStreaming']);

      // A sender that ends by itself releases it too.
      await sender.start();
      await settle();
      fake.emitSenderStatus(
        const SenderStatusDto(
          state: 'failed',
          error: 'the hub went away',
          bitrate: 0,
          lossPct: 0,
          rttMs: 0,
          levelDb: -120,
          hubGain: 1,
          hubMuted: false,
          hubPriority: false,
        ),
      );
      await settle();
      expect(native.calls, [
        'beginStreaming',
        'endStreaming',
        'beginStreaming',
        'endStreaming',
      ]);
    });

    test('other desktops do not ask for it', () async {
      final fake = FakeHfaApi(trusted: [trustedPeer]);
      final native = RecordingNativeChannel();
      final c = containerFor(fake, native: native);
      c.listen(senderControllerProvider, (_, _) {});
      final sender = c.read(senderControllerProvider.notifier);
      sender.selectTarget(HubTarget.discovered(trustedHub));
      sender.selectSource(const SourceChoice(SourceKind.tone));
      await sender.start();
      await sender.stop();
      expect(native.calls, isNot(contains('beginStreaming')));
    });

    test(
      'the last hub and source are remembered and preselected next time',
      () async {
        final dir = await Directory.systemTemp.createTemp('hfa_last_');
        addTearDown(() => dir.delete(recursive: true));
        final fake = FakeHfaApi(
          trusted: [trustedPeer],
          captureApps: [const CaptureAppDto(pid: 7, name: 'Firefox')],
        );
        Future<ProviderContainer> launch() async {
          final c = containerFor(fake, dataDir: dir.path);
          c.listen(senderControllerProvider, (_, _) {});
          c.listen(appPrefsProvider, (_, _) {});
          await c.read(appPrefsProvider.notifier).flush();
          // Trust store and app list are asked asynchronously.
          await Future<void>.delayed(const Duration(milliseconds: 50));
          return c;
        }

        var c = await launch();
        expect(c.read(senderControllerProvider).target, isNull);
        final sender = c.read(senderControllerProvider.notifier);
        sender.selectTarget(HubTarget.discovered(trustedHub));
        sender.selectSource(
          const SourceChoice(
            SourceKind.app,
            app: CaptureAppDto(pid: 7, name: 'Firefox'),
          ),
        );
        await sender.start();
        await settle();
        final prefs = c.read(appPrefsProvider);
        expect(prefs.lastHub?.deviceId, trustedHub.deviceId);
        expect(prefs.lastHub?.name, 'Desk PC');
        expect(
          prefs.lastSource,
          const LastSource(SourceKind.app, appName: 'Firefox'),
        );
        await sender.stop();
        await c.read(appPrefsProvider.notifier).flush();
        c.dispose();

        // Next launch: Firefox runs with another pid.
        fake.captureApps
          ..clear()
          ..add(const CaptureAppDto(pid: 99, name: 'Firefox'));
        c = await launch();
        var state = c.read(senderControllerProvider);
        expect(state.target?.deviceId, trustedHub.deviceId);
        expect(state.target?.trusted, isTrue);
        expect(state.target?.needsPin, isFalse);
        expect(
          state.source,
          const SourceChoice(
            SourceKind.app,
            app: CaptureAppDto(pid: 99, name: 'Firefox'),
          ),
        );
        c.dispose();

        // Forgotten since: the remembered hub asks for a PIN.
        fake.trusted.clear();
        c = await launch();
        state = c.read(senderControllerProvider);
        expect(state.target?.deviceId, trustedHub.deviceId);
        expect(state.target?.trusted, isFalse);
        expect(state.target?.needsPin, isTrue);
        c.dispose();
      },
    );

    test('a choice made before the prefs loaded is not overwritten', () async {
      final fake = FakeHfaApi(trusted: [trustedPeer]);
      final c = containerFor(fake);
      c.listen(senderControllerProvider, (_, _) {});
      final sender = c.read(senderControllerProvider.notifier);
      sender.selectTarget(HubTarget.discovered(strangerHub));
      c
          .read(appPrefsProvider.notifier)
          .rememberSend(
            HubTarget.discovered(trustedHub),
            const SourceChoice(SourceKind.tone),
          );
      await settle();
      expect(
        c.read(senderControllerProvider).target?.deviceId,
        strangerHub.deviceId,
      );
      // "Send to …" takes the remembered one on request.
      expect(await sender.useRemembered(), isTrue);
      final state = c.read(senderControllerProvider);
      expect(state.target?.deviceId, trustedHub.deviceId);
      expect(state.target?.trusted, isTrue);
      expect(state.source, const SourceChoice(SourceKind.tone));
    });
  });

  group('DiscoveryController', () {
    test('tracks found/lost hubs and holds the multicast lock', () async {
      final fake = FakeHfaApi(discoverableHubs: [trustedHub]);
      final native = RecordingNativeChannel();
      final c = containerFor(fake, native: native);
      final sub = c.listen(discoveryControllerProvider, (_, _) {});
      await settle();
      expect(c.read(discoveryControllerProvider).hubs.keys, [
        trustedHub.deviceId,
      ]);
      expect(native.calls, contains('acquireMulticastLock'));

      fake.emitDiscovery(const DiscoveryEventDto.found(strangerHub));
      await settle();
      expect(c.read(discoveryControllerProvider).hubs, hasLength(2));
      fake.emitDiscovery(DiscoveryEventDto.lost(deviceId: trustedHub.deviceId));
      await settle();
      expect(c.read(discoveryControllerProvider).hubs.keys, [
        strangerHub.deviceId,
      ]);

      sub.close();
      await settle(); // autoDispose
      await settle();
      expect(fake.calls, contains('stopDiscovery'));
      expect(native.calls, contains('releaseMulticastLock'));
      expect(fake.discovering, isFalse);
    });

    test('a new browse waits for the previous screen\'s stop', () async {
      final fake = _SlowStopApi(discoverableHubs: [trustedHub]);
      final c = containerFor(fake);
      var sub = c.listen(discoveryControllerProvider, (_, _) {});
      await settle();
      sub.close();
      await settle(); // autoDispose: the stop is now in flight
      await settle();
      expect(fake.calls, contains('stopDiscovery'));

      // The sender screen is entered again before the stop finished.
      sub = c.listen(discoveryControllerProvider, (_, _) {});
      await settle();
      expect(fake.calls.where((m) => m == 'discoverHubs'), hasLength(1));
      fake.stopGate.complete();
      await settle();
      await settle();
      expect(fake.calls.where((m) => m == 'discoverHubs'), hasLength(2));
      expect(fake.discovering, isTrue);
      expect(c.read(discoveryControllerProvider).hubs, hasLength(1));
      expect(c.read(discoveryControllerProvider).error, isNull);
      sub.close();
    });

    test('a browse the core ends is reported', () async {
      final fake = FakeHfaApi(discoverableHubs: [trustedHub]);
      final c = containerFor(fake);
      c.listen(discoveryControllerProvider, (_, _) {});
      await settle();
      await fake.stopDiscovery(); // e.g. another caller stopped it
      await settle();
      expect(c.read(discoveryControllerProvider).error, contains('refresh'));
    });
  });

  group('AppPrefs', () {
    test(
      'the hub starts on launch when asked, and the choice is kept',
      () async {
        final dir = await Directory.systemTemp.createTemp('hfa_prefs_');
        addTearDown(() => dir.delete(recursive: true));
        ProviderContainer launch(FakeHfaApi fake) {
          final c = ProviderContainer.test(
            overrides: [
              ...overridesFor(fake),
              dataDirProvider.overrideWithValue(dir.path),
            ],
            retry: (retryCount, error) => null,
          );
          c.listen(hubControllerProvider, (_, _) {});
          c.listen(appPrefsProvider, (_, _) {});
          return c;
        }

        var fake = FakeHfaApi();
        var c = launch(fake);
        await c.read(appPrefsProvider.notifier).flush();
        expect(fake.hubRunning, isFalse);
        c.read(appPrefsProvider.notifier).setStartHubOnLaunch(true);
        await c.read(appPrefsProvider.notifier).flush();
        // Only at launch: switching it on does not start the hub now.
        expect(fake.hubRunning, isFalse);
        c.dispose();

        fake = FakeHfaApi();
        c = launch(fake);
        await c.read(appPrefsProvider.notifier).flush();
        expect(c.read(appPrefsProvider).startHubOnLaunch, isTrue);
        expect(fake.hubRunning, isTrue);
        expect(c.read(hubControllerProvider).running, isTrue);
        c.dispose();
      },
    );

    test('a broken prefs file means the defaults', () {
      expect(AppPrefs.fromJson('nonsense').startHubOnLaunch, isFalse);
      expect(
        AppPrefs.fromJson({'startHubOnLaunch': 'yes'}).startHubOnLaunch,
        isFalse,
      );
      expect(
        AppPrefs.fromJson({'startHubOnLaunch': true}).startHubOnLaunch,
        isTrue,
      );
    });
  });

  group('Settings', () {
    test('save validates in the core and renames the device', () async {
      final fake = FakeHfaApi(deviceName: 'Old');
      final c = containerFor(fake);
      c.listen(settingsControllerProvider, (_, _) {});
      c.listen(appInfoProvider, (_, _) {});
      final initial = await c.read(settingsControllerProvider.future);
      await c
          .read(settingsControllerProvider.notifier)
          .save(
            SettingsDto(
              deviceName: '  New name ',
              port: initial.port,
              bitrate: 96000,
              frameMs: 20,
              fec: false,
              jitterMinMs: 30,
              jitterMaxMs: 300,
            ),
          );
      expect(c.read(settingsControllerProvider).value?.deviceName, 'New name');
      expect(c.read(appInfoProvider).deviceName, 'New name');
      expect(fake.settings.frameMs, 20);

      await expectLater(
        c
            .read(settingsControllerProvider.notifier)
            .save(
              const SettingsDto(
                deviceName: 'x',
                port: 0,
                bitrate: 1,
                frameMs: 10,
                fec: true,
                jitterMinMs: 10,
                jitterMaxMs: 20,
              ),
            ),
        throwsA(isA<HfaApiException>()),
      );
      expect(c.read(settingsControllerProvider).value?.bitrate, 96000);
    });

    test(
      'forget leaves a running hub running (the core drops the peer)',
      () async {
        final fake = FakeHfaApi(
          trusted: [
            const TrustedPeerDto(
              deviceId: 'a',
              name: 'A',
              pairedAtUnix: 1,
              pairedAsHub: true,
              pairedAsSender: true,
            ),
          ],
        );
        final c = containerFor(fake);
        c.listen(trustedPeersProvider, (_, _) {});
        c.listen(hubControllerProvider, (_, _) {});
        await c.read(hubControllerProvider.notifier).start();
        fake.calls.clear();
        await c.read(trustedPeersProvider.notifier).forget('a');
        expect(fake.calls, contains('forgetPeer'));
        expect(fake.calls, isNot(contains('hubStop')));
        expect(c.read(hubControllerProvider).running, isTrue);
      },
    );

    test('forget stops a live sender that streams to that hub', () async {
      final fake = FakeHfaApi(trusted: [trustedPeer]);
      final c = containerFor(fake);
      c.listen(trustedPeersProvider, (_, _) {});
      c.listen(senderControllerProvider, (_, _) {});
      final sender = c.read(senderControllerProvider.notifier);
      sender.selectTarget(HubTarget.discovered(trustedHub));
      await sender.start();
      await settle();
      expect(c.read(senderControllerProvider).isLive, isTrue);
      await c.read(trustedPeersProvider.notifier).forget(trustedHub.deviceId);
      await settle();
      expect(fake.calls, containsAllInOrder(['forgetPeer', 'senderStop']));
      expect(c.read(senderControllerProvider).isLive, isFalse);
    });

    test('forget removes the peer and reloads the list', () async {
      final fake = FakeHfaApi(
        trusted: [
          const TrustedPeerDto(
            deviceId: 'a',
            name: 'A',
            pairedAtUnix: 1,
            pairedAsHub: true,
            pairedAsSender: true,
          ),
          const TrustedPeerDto(
            deviceId: 'b',
            name: 'B',
            pairedAtUnix: 2,
            pairedAsHub: true,
            pairedAsSender: true,
          ),
        ],
      );
      final c = containerFor(fake);
      c.listen(trustedPeersProvider, (_, _) {});
      expect(await c.read(trustedPeersProvider.future), hasLength(2));
      await c.read(trustedPeersProvider.notifier).forget('a');
      expect(c.read(trustedPeersProvider).value?.map((p) => p.deviceId), ['b']);
    });
  });
}

class _FailingHubApi extends FakeHfaApi {
  @override
  Future<HubStatusDto> hubStart() async {
    throw const HfaApiException('no output device');
  }
}

/// Refuses master gains above 1 while the hub runs.
class _MasterGainFailingApi extends FakeHfaApi {
  @override
  Future<void> hubSetMasterGain(double gain) async {
    if (hubRunning && gain > 1) throw const HfaApiException('mixer busy');
    return super.hubSetMasterGain(gain);
  }
}

/// `hubStartPairing` resolves only when [open] completes.
class _SlowPairingApi extends FakeHfaApi {
  Completer<void> open = Completer<void>();

  @override
  Future<PairingInfoDto> hubStartPairing() async {
    final gate = open;
    final info = await super.hubStartPairing();
    await gate.future;
    // Like the core: the window exists now, whatever was cancelled before.
    pairing = info;
    return info;
  }
}

/// A core whose `stopDiscovery` takes until [stopGate] completes.
class _SlowStopApi extends FakeHfaApi {
  _SlowStopApi({super.discoverableHubs});

  final Completer<void> stopGate = Completer<void>();

  @override
  Future<void> stopDiscovery() async {
    calls.add('stopDiscovery');
    await stopGate.future;
    await super.stopDiscovery();
    calls.removeLast();
  }
}

/// A native channel whose consent dialog stays open until [consent]
/// completes.
class _ConsentNativeChannel extends RecordingNativeChannel {
  final Completer<bool> consent = Completer<bool>();

  @override
  Future<bool> startSystemCapture({
    required int feedId,
    required int sampleRate,
    required int channels,
  }) {
    calls.add('startSystemCapture');
    return consent.future;
  }
}

/// A native channel whose RECORD_AUDIO permission is refused.
class _PermissionDeniedNativeChannel extends RecordingNativeChannel {
  @override
  Future<bool> startSystemCapture({
    required int feedId,
    required int sampleRate,
    required int channels,
  }) async {
    calls.add('startSystemCapture');
    throw PlatformException(
      code: 'permissionDenied',
      message:
          'Capturing this device\'s audio needs the audio recording '
          'permission.',
    );
  }
}

/// A subscription that only ever gets the initial status (every later
/// status event is lost), as when a change races the subscription.
class _SilentSenderEventsApi extends FakeHfaApi {
  _SilentSenderEventsApi({super.platform});

  @override
  Stream<SenderStatusDto> senderEvents() {
    calls.add('senderEvents');
    return Stream<SenderStatusDto>.multi((controller) {
      controller.add(senderStatusNow);
    }, isBroadcast: true);
  }
}
