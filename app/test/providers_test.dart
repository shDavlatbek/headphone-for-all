import 'dart:async';

import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:headphone_for_all/src/api/hfa_api.dart';
import 'package:headphone_for_all/src/models/hub_target.dart';
import 'package:headphone_for_all/src/models/source_choice.dart';
import 'package:headphone_for_all/src/platform/native_channel.dart';
import 'package:headphone_for_all/src/state/core_providers.dart';
import 'package:headphone_for_all/src/state/discovery_controller.dart';
import 'package:headphone_for_all/src/state/hub_controller.dart';
import 'package:headphone_for_all/src/state/pairing_controller.dart';
import 'package:headphone_for_all/src/state/sender_controller.dart';
import 'package:headphone_for_all/src/state/settings_controller.dart';

import 'helpers.dart';

/// Lets stream events and microtasks run.
Future<void> settle() => Future<void>.delayed(Duration.zero);

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
      'master gain is remembered while stopped and applied on start',
      () async {
        final fake = FakeHfaApi();
        final c = containerFor(fake);
        c.listen(hubControllerProvider, (_, _) {});
        final hub = c.read(hubControllerProvider.notifier);
        await hub.setMasterGain(1.5);
        expect(fake.calls, isNot(contains('hubSetMasterGain')));
        await hub.start();
        expect(fake.masterGain, 1.5);
        await hub.setMasterGain(0.25);
        expect(fake.masterGain, 0.25);
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

    test(
      'a failure after hubStart keeps core, service and UI agreeing',
      () async {
        final fake = _MasterGainFailingApi();
        final native = RecordingNativeChannel();
        final c = containerFor(fake, native: native);
        // Errors are transient (the hub screen shows each in a snack bar).
        final errors = <String>[];
        c.listen(hubControllerProvider.select((h) => h.error), (_, error) {
          if (error != null) errors.add(error);
        });
        final hub = c.read(hubControllerProvider.notifier);
        await hub.setMasterGain(2);
        await hub.start();
        final state = c.read(hubControllerProvider);
        expect(fake.hubRunning, isTrue);
        expect(state.running, isTrue);
        expect(errors, ['mixer busy']);
        expect(state.masterGain, 1);
        expect(native.calls, ['startHubService']);
      },
    );

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
        expect(native.calls, ['startSystemCapture']);
        expect(fake.senderStatusNow.state, 'failed');
        native.consent.complete(true);
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

    test('forget restarts a running hub so it drops the peer', () async {
      final fake = FakeHfaApi(
        trusted: [
          const TrustedPeerDto(deviceId: 'a', name: 'A', pairedAtUnix: 1),
        ],
      );
      final c = containerFor(fake);
      c.listen(trustedPeersProvider, (_, _) {});
      c.listen(hubControllerProvider, (_, _) {});
      await c.read(hubControllerProvider.notifier).start();
      fake.calls.clear();
      final restarted = await c.read(trustedPeersProvider.notifier).forget('a');
      expect(restarted, isTrue);
      expect(
        fake.calls,
        containsAllInOrder(['forgetPeer', 'hubStop', 'hubStart']),
      );
      expect(c.read(hubControllerProvider).running, isTrue);
    });

    test('forget removes the peer and reloads the list', () async {
      final fake = FakeHfaApi(
        trusted: [
          const TrustedPeerDto(deviceId: 'a', name: 'A', pairedAtUnix: 1),
          const TrustedPeerDto(deviceId: 'b', name: 'B', pairedAtUnix: 2),
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

class _MasterGainFailingApi extends FakeHfaApi {
  @override
  Future<void> hubSetMasterGain(double gain) async {
    if (hubRunning) throw const HfaApiException('mixer busy');
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
