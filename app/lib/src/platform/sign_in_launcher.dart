/// Starting the app when the user signs in, hidden in the tray
/// (`--autostart`, docs/CONTRACTS.md §8.10).
///
/// - **Linux:** an XDG autostart entry
///   (`$XDG_CONFIG_HOME/autostart/io.github.shdavlatbek.hfa.desktop`, run by
///   GNOME, KDE, Xfce, … at sign-in) whose `Exec` is this executable (the
///   `.AppImage` file itself for an AppImage) with `--autostart`. Inside
///   Flatpak the sandbox cannot write the host's autostart folder, so the
///   request goes through the Background portal
///   (`org.freedesktop.portal.Background.RequestBackground`, `autostart`).
/// - **Windows:** the installer's "start in the notification area when I sign
///   in" task (a Startup shortcut with `--autostart`); not switched here.
/// - **macOS:** System Settings → General → Login Items (documented; the app
///   registers no login item itself).
library;

import 'dart:async';
import 'dart:io';

import 'package:dbus/dbus.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../state/app_prefs.dart';

/// The argument that starts the app hidden in the tray.
const autostartArgument = '--autostart';

/// The application id (desktop file name, icon name).
const linuxAppId = 'io.github.shdavlatbek.hfa';

/// Turns starting at sign-in on and off.
abstract class SignInLauncher {
  /// Whether the app starts at sign-in, or `null` when that cannot be read
  /// back (the Flatpak portal): the app's own record is shown then.
  Future<bool?> isEnabled();

  /// Starts (or stops starting) the app at sign-in. Throws when that fails.
  Future<void> setEnabled(bool enabled);
}

/// The command that starts this app: `$APPIMAGE` when running from an
/// AppImage (the executable itself lives in a mount that changes every run),
/// else [resolvedExecutable].
String linuxLaunchCommand(Map<String, String> env, String resolvedExecutable) {
  final appImage = env['APPIMAGE'];
  return appImage != null && appImage.isNotEmpty
      ? appImage
      : resolvedExecutable;
}

/// Quotes [arg] for a desktop entry's `Exec` key (Desktop Entry
/// Specification, "The Exec key"): arguments with reserved characters are
/// double-quoted with `"`, `` ` ``, `$` and `\` escaped; `%` is doubled.
///
/// The key's value is a string, and the spec applies the string escape rule
/// before the quoting rule: every backslash of the quoted form is written
/// twice (a literal `\` in a quoted argument becomes four backslashes, a
/// literal `$` becomes `\\$`), and a tab, newline or carriage return is
/// written as `\t`, `\n` or `\r`. Without that pass a key-file parser
/// (GLib) rejects `\"` and `\$` as invalid escapes and ignores the entry.
String desktopExecQuote(String arg) {
  final percent = arg.replaceAll('%', '%%');
  if (!RegExp(r'''[\s"'\\><~|&;$*?#()`]''').hasMatch(percent)) return percent;
  final escaped = percent.replaceAllMapped(
    RegExp(r'["`$\\]'),
    (m) => '\\${m[0]}',
  );
  return _desktopStringEscape('"$escaped"');
}

/// The Desktop Entry string escape (`\\`, `\t`, `\n`, `\r`) of [value].
String _desktopStringEscape(String value) => value.replaceAllMapped(
  RegExp('[\\\\\t\n\r]'),
  (m) => switch (m[0]) {
    '\t' => r'\t',
    '\n' => r'\n',
    '\r' => r'\r',
    _ => r'\\',
  },
);

/// The XDG autostart entry that starts [command] hidden in the tray.
String autostartDesktopEntry(String command) =>
    '''
[Desktop Entry]
Type=Application
Version=1.5
Name=Headphone for All
Comment=Start Headphone for All in the tray at sign-in
Exec=${desktopExecQuote(command)} $autostartArgument
Icon=$linuxAppId
Terminal=false
NoDisplay=true
X-GNOME-Autostart-enabled=true
''';

/// The XDG autostart folder: `$XDG_CONFIG_HOME/autostart`, else
/// `~/.config/autostart`; `null` without a home directory.
String? xdgAutostartDir(Map<String, String> env) {
  final config = env['XDG_CONFIG_HOME'];
  if (config != null && config.startsWith('/')) return '$config/autostart';
  final home = env['HOME'];
  if (home == null || home.isEmpty) return null;
  return '$home/.config/autostart';
}

/// A Linux XDG autostart entry in [autostartDir].
class XdgSignInLauncher implements SignInLauncher {
  /// Creates the launcher for [command] (see [linuxLaunchCommand]).
  XdgSignInLauncher({required this.autostartDir, required this.command});

  /// The autostart folder (see [xdgAutostartDir]).
  final String autostartDir;

  /// What the entry runs (with [autostartArgument]).
  final String command;

  /// The entry's file.
  File get file => File('$autostartDir/$linuxAppId.desktop');

  @override
  Future<bool?> isEnabled() async {
    try {
      if (!await file.exists()) return false;
      // Another tool may have disabled the entry.
      final text = await file.readAsString();
      bool has(String line) =>
          RegExp('^\\s*$line\\s*\$', multiLine: true).hasMatch(text);
      return !has(r'Hidden\s*=\s*true') &&
          !has(r'X-GNOME-Autostart-enabled\s*=\s*false');
    } on FileSystemException {
      return false;
    }
  }

  @override
  Future<void> setEnabled(bool enabled) async {
    if (enabled) {
      await Directory(autostartDir).create(recursive: true);
      await file.writeAsString(autostartDesktopEntry(command), flush: true);
    } else if (await file.exists()) {
      await file.delete();
    }
  }
}

/// Flatpak: asks the Background portal to start the app at sign-in. The
/// portal cannot be asked whether it will, so [isEnabled] is `null`.
class FlatpakSignInLauncher implements SignInLauncher {
  /// Creates the launcher.
  const FlatpakSignInLauncher();

  @override
  Future<bool?> isEnabled() async => null;

  @override
  Future<void> setEnabled(bool enabled) async {
    final client = DBusClient.session();
    try {
      final portal = DBusRemoteObject(
        client,
        name: 'org.freedesktop.portal.Desktop',
        path: DBusObjectPath('/org/freedesktop/portal/desktop'),
      );
      // The answer arrives later as a Response signal on the returned
      // request; the portal may ask the user first. Nothing waits for it.
      await portal.callMethod(
        'org.freedesktop.portal.Background',
        'RequestBackground',
        [
          const DBusString(''),
          DBusDict.stringVariant({
            'reason': const DBusString(
              'Start Headphone for All in the tray when you sign in',
            ),
            'autostart': DBusBoolean(enabled),
            'commandline': DBusArray.string([
              'headphone_for_all',
              autostartArgument,
            ]),
            'dbus-activatable': const DBusBoolean(false),
          }),
        ],
        replySignature: DBusSignature('o'),
      );
    } finally {
      unawaited(client.close());
    }
  }
}

/// The launcher of this platform, or `null` where the app does not switch
/// starting at sign-in itself (Windows: installer; macOS: Login Items;
/// mobile).
SignInLauncher? platformSignInLauncher() {
  if (kIsWeb || !Platform.isLinux) return null;
  if (File('/.flatpak-info').existsSync()) return const FlatpakSignInLauncher();
  final env = Platform.environment;
  final dir = xdgAutostartDir(env);
  if (dir == null) return null;
  return XdgSignInLauncher(
    autostartDir: dir,
    command: linuxLaunchCommand(env, Platform.resolvedExecutable),
  );
}

/// How this device starts the app at sign-in (`null`: not switchable here).
/// Overridden in tests.
final signInLauncherProvider = Provider<SignInLauncher?>(
  (ref) => platformSignInLauncher(),
);

/// Whether the app starts at sign-in (Linux), from the autostart entry or,
/// where that cannot be read (Flatpak), from the app's own record.
final startAtSignInProvider = AsyncNotifierProvider<StartAtSignIn, bool>(
  StartAtSignIn.new,
);

/// Reads and switches [signInLauncherProvider].
class StartAtSignIn extends AsyncNotifier<bool> {
  @override
  Future<bool> build() async {
    final launcher = ref.watch(signInLauncherProvider);
    if (launcher == null) return false;
    final enabled = await launcher.isEnabled();
    if (enabled != null) return enabled;
    await ref.read(appPrefsProvider.notifier).loaded;
    return ref.read(appPrefsProvider).startAtSignIn;
  }

  /// Starts (or stops starting) the app at sign-in. Throws when that fails
  /// (the switch keeps its previous position).
  Future<void> set(bool enabled) async {
    final launcher = ref.read(signInLauncherProvider);
    if (launcher == null) return;
    await launcher.setEnabled(enabled);
    ref.read(appPrefsProvider.notifier).setStartAtSignIn(enabled);
    if (ref.mounted) state = AsyncData(enabled);
  }
}
