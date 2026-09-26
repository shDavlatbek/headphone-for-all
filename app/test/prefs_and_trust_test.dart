import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:headphone_for_all/src/api/hfa_api.dart';
import 'package:headphone_for_all/src/models/hub_target.dart';
import 'package:headphone_for_all/src/models/source_choice.dart';
import 'package:headphone_for_all/src/platform/native_channel.dart';
import 'package:headphone_for_all/src/platform/sign_in_launcher.dart';
import 'package:headphone_for_all/src/screens/sender_screen.dart';
import 'package:headphone_for_all/src/screens/settings_screen.dart';
import 'package:headphone_for_all/src/state/app_prefs.dart';
import 'package:headphone_for_all/src/state/sender_controller.dart';

SenderStatusDto _status({
  double gain = 1,
  bool muted = false,
  bool priority = false,
}) => SenderStatusDto(
  state: 'streaming',
  bitrate: 128000,
  lossPct: 0,
  rttMs: 3,
  levelDb: -20,
  hubGain: gain,
  hubMuted: muted,
  hubPriority: priority,
);

void main() {
  group('AppPrefs', () {
    test('round-trips the remembered hub and source', () {
      const prefs = AppPrefs(
        startHubOnLaunch: true,
        startAtSignIn: true,
        lastHub: LastHub(
          name: 'Desk PC',
          deviceId: 'ab12-cd34-ef56-7890',
          host: '192.168.1.20',
          port: 47810,
          hubKey: 'AQID',
          direct: true,
        ),
        lastSource: LastSource(SourceKind.app, appName: 'Firefox'),
      );
      expect(AppPrefs.fromJson(prefs.toJson()), prefs);
      // Written before these fields existed.
      expect(
        AppPrefs.fromJson({'startHubOnLaunch': true}),
        const AppPrefs(startHubOnLaunch: true),
      );
    });

    test('broken remembered entries are dropped, the rest is kept', () {
      final prefs = AppPrefs.fromJson({
        'startHubOnLaunch': true,
        'startAtSignIn': 'yes',
        'lastHub': {'name': 'Desk', 'host': '', 'port': 47810},
        'lastSource': {'kind': 'hologram'},
      });
      expect(prefs.startHubOnLaunch, isTrue);
      expect(prefs.startAtSignIn, isFalse);
      expect(prefs.lastHub, isNull, reason: 'no id and no address');
      expect(prefs.lastSource, isNull);
      for (final hub in [
        {'name': ' ', 'host': 'h', 'port': 1},
        {'name': 'x', 'host': 'h', 'port': 70000},
        {'name': 'x', 'host': 7, 'port': 1},
        {'name': 'x', 'deviceId': '', 'host': 'h', 'port': 1},
        'junk',
      ]) {
        expect(LastHub.fromJson(hub), isNull, reason: '$hub');
      }
      expect(
        LastHub.fromJson({
          'name': 'x',
          'deviceId': 'id',
          'host': '',
          'port': 0,
        }),
        const LastHub(name: 'x', deviceId: 'id'),
      );
    });

    test('a remembered hub is trusted only when the trust store says so', () {
      final hub = LastHub.of(
        const HubTarget(
          name: 'Desk PC',
          origin: HubOrigin.pairingLink,
          host: '10.0.0.9',
          port: 47810,
          deviceId: 'id',
          hubKey: 'key',
          pairingSecret: 'one-time token',
          trusted: true,
        ),
      );
      expect(hub.toJson().values, isNot(contains('one-time token')));
      expect(hub.direct, isTrue, reason: 'reached at its address');
      final untrusted = hub.toTarget(trusted: false);
      expect(untrusted.origin, HubOrigin.paired);
      expect(untrusted.address, '10.0.0.9:47810');
      expect(untrusted.hubKey, 'key');
      expect(untrusted.needsPin, isTrue);
      expect(hub.toTarget(trusted: true).needsPin, isFalse);
      // Typed in by address, id unknown: the core decides.
      final manual = const LastHub(name: '10.0.0.9', host: '10.0.0.9');
      expect(manual.toTarget(trusted: false).origin, HubOrigin.manual);
      expect(manual.toTarget(trusted: false).needsPin, isFalse);
    });

    test('a hub found on the LAN is remembered by id, not by address', () {
      const hub = HubInfoDto(
        deviceId: 'hub1',
        name: 'Desk',
        addrs: ['192.168.1.20'],
        port: 47810,
        platform: 'windows',
        trusted: true,
      );
      final found = LastHub.of(HubTarget.discovered(hub));
      expect(found.direct, isFalse);
      expect(found.host, '192.168.1.20', reason: 'kept for reference');
      final target = found.toTarget(trusted: true);
      expect(target.origin, HubOrigin.paired);
      expect((target.host, target.port), ('', 0), reason: 'found by id');
      expect(LastHub.fromJson(found.toJson()), found);
      // A hub added by address and paired (it may not be announced).
      final typed = LastHub.of(
        const HubTarget(
          name: 'Desk',
          origin: HubOrigin.paired,
          host: '192.168.1.44',
          port: 47811,
          deviceId: 'hub1',
          trusted: true,
        ),
      );
      expect(typed.direct, isTrue);
      expect(typed.toTarget(trusted: true).address, '192.168.1.44:47811');
    });

    test('a remembered app is found again by name', () {
      const source = LastSource(SourceKind.app, appName: 'Spotify');
      expect(source.toChoice().isComplete, isFalse);
      expect(
        source.toChoice(const [
          CaptureAppDto(pid: 1, name: 'Firefox'),
          CaptureAppDto(pid: 2, name: 'Spotify'),
        ]),
        const SourceChoice(
          SourceKind.app,
          app: CaptureAppDto(pid: 2, name: 'Spotify'),
        ),
      );
      expect(
        const LastSource(SourceKind.tone).toChoice(),
        const SourceChoice(SourceKind.tone),
      );
    });
  });

  group('trust roles', () {
    const hub = HubInfoDto(
      deviceId: 'hub1',
      name: 'Desk',
      addrs: ['10.0.0.2'],
      port: 47810,
      platform: 'linux',
      trusted: false,
    );

    test('a peer that only paired as a sender is not a trusted hub', () {
      final senderOnly = FakeHfaApi.peer(
        deviceId: 'hub1',
        asHub: false,
        asSender: true,
      );
      final paired = HubTarget.paired(senderOnly);
      expect(paired.trusted, isFalse);
      expect(paired.needsPin, isTrue);
      final fresh = currentHubTarget(
        HubTarget.discovered(hub),
        discovered: {'hub1': hub},
        peers: [senderOnly],
      );
      expect(fresh.trusted, isFalse);
      expect(fresh.needsPin, isTrue);
      final asHub = currentHubTarget(
        HubTarget.discovered(hub),
        discovered: {'hub1': hub},
        peers: [FakeHfaApi.peer(deviceId: 'hub1')],
      );
      expect(asHub.trusted, isTrue);
    });

    test('a paired hub forgotten since loses its trust', () {
      final target = HubTarget.paired(FakeHfaApi.peer(deviceId: 'hub1'));
      final forgotten = currentHubTarget(
        target,
        discovered: const {},
        peers: const [],
      );
      expect(forgotten.trusted, isFalse);
      expect(forgotten.needsPin, isTrue);
      // While the trust store is still loading nothing changes.
      expect(
        currentHubTarget(target, discovered: const {}, peers: null),
        target,
      );
      // An entered PIN is kept.
      expect(
        currentHubTarget(
          target.withPin('123456'),
          discovered: const {},
          peers: const [],
        ).pairingSecret,
        '123456',
      );
    });
  });

  group('sender card', () {
    test('hub control chips say how the hub plays this stream', () {
      expect(hubControlChips(_status()), isEmpty);
      expect(
        hubControlChips(_status(muted: true, gain: 0.5)).map((c) => c.label),
        ['Muted on the hub'],
      );
      expect(
        hubControlChips(_status(gain: 0.5)).single.label,
        'Volume 50% on the hub',
      );
      expect(hubControlChips(_status(gain: 1.004)), isEmpty);
      expect(
        hubControlChips(_status(gain: 1.5, priority: true)).map((c) => c.key),
        ['hub-volume-chip', 'hub-priority-chip'],
      );
    });

    test('the broadcast title follows the extension state', () {
      SenderState with_(BroadcastState s) => SenderState(
        status: idleSenderStatus,
        broadcast: BroadcastStatus(state: s),
      );
      expect(broadcastTitle(with_(BroadcastState.idle)), isNull);
      expect(
        broadcastTitle(with_(BroadcastState.connecting)),
        'Broadcast connecting…',
      );
      expect(broadcastTitle(with_(BroadcastState.streaming)), 'Broadcasting');
      expect(
        broadcastTitle(with_(BroadcastState.reconnecting)),
        'Broadcast reconnecting…',
      );
      expect(broadcastTitle(with_(BroadcastState.failed)), 'Broadcast failed');
      expect(
        broadcastTitle(with_(BroadcastState.stopped)),
        'Broadcast stopped',
      );
      // Older extensions only say "started".
      expect(
        broadcastTitle(
          const SenderState(status: idleSenderStatus, broadcasting: true),
        ),
        'Broadcasting',
      );
    });
  });

  group('start at sign-in', () {
    test('the autostart entry runs this app hidden', () {
      final entry = autostartDesktopEntry(
        '/opt/Headphone for All/headphone_for_all',
      );
      expect(entry, startsWith('[Desktop Entry]\n'));
      expect(
        entry,
        contains(
          'Exec="/opt/Headphone for All/headphone_for_all" --autostart\n',
        ),
      );
      expect(entry, contains('Icon=io.github.shdavlatbek.hfa\n'));
      expect(entry, contains('X-GNOME-Autostart-enabled=true\n'));
    });

    test('Exec arguments are quoted per the Desktop Entry spec', () {
      expect(desktopExecQuote('/usr/bin/hfa'), '/usr/bin/hfa');
      // The quoting rule, then the string escape rule (every `\` doubled):
      // `\\$` for `$`, `\\"` for `"`, four backslashes for `\`.
      expect(desktopExecQuote(r'/a b/$x"`\y'), r'"/a b/\\$x\\"\\`\\\\y"');
      expect(
        desktopExecQuote(r'/home/me/My $Apps/hfa'),
        r'"/home/me/My \\$Apps/hfa"',
      );
      expect(desktopExecQuote(r'C:\hfa'), r'"C:\\\\hfa"');
      expect(desktopExecQuote('/a\tb/hfa'), r'"/a\tb/hfa"');
      expect(desktopExecQuote('/a b/hfa'), '"/a b/hfa"');
      expect(desktopExecQuote('/50%/hfa'), '/50%%/hfa');
    });

    test('an AppImage starts from its .AppImage file', () {
      expect(
        linuxLaunchCommand({
          'APPIMAGE': '/home/me/Hfa.AppImage',
        }, '/tmp/.mount_x/hfa'),
        '/home/me/Hfa.AppImage',
      );
      expect(linuxLaunchCommand({}, '/usr/lib/hfa/hfa'), '/usr/lib/hfa/hfa');
      expect(
        xdgAutostartDir({'XDG_CONFIG_HOME': '/cfg', 'HOME': '/h'}),
        '/cfg/autostart',
      );
      expect(
        xdgAutostartDir({'XDG_CONFIG_HOME': 'relative', 'HOME': '/h'}),
        '/h/.config/autostart',
      );
      expect(xdgAutostartDir({}), isNull);
    });

    test('the XDG entry is written, read back and removed', () async {
      final dir = await Directory.systemTemp.createTemp('hfa_autostart_');
      addTearDown(() => dir.delete(recursive: true));
      final launcher = XdgSignInLauncher(
        autostartDir: '${dir.path}/autostart',
        command: '/usr/bin/headphone_for_all',
      );
      expect(await launcher.isEnabled(), isFalse);
      await launcher.setEnabled(true);
      expect(await launcher.isEnabled(), isTrue);
      expect(
        await launcher.file.readAsString(),
        contains('Exec=/usr/bin/headphone_for_all --autostart'),
      );
      // Switched off by the desktop's own startup settings.
      await launcher.file.writeAsString(
        '${await launcher.file.readAsString()}Hidden=true\n',
      );
      expect(await launcher.isEnabled(), isFalse);
      await launcher.setEnabled(false);
      expect(await launcher.file.exists(), isFalse);
      await launcher.setEnabled(false);
    });

    test('Windows and macOS explain where it is switched', () {
      expect(signInHint('windows'), contains('installer'));
      expect(signInHint('macos'), contains('Login Items'));
      expect(signInHint('linux'), isNull);
      expect(signInHint('android'), isNull);
    });
  });
}
