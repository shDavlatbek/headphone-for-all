/// Desktop (Windows, macOS, Linux) shell integration: a tray icon with
/// Show/Hide, Hub on/off and Quit, and "close hides to the tray" while the
/// hub or the sender runs.
library;

import 'dart:async';
import 'dart:io';

import 'package:dbus/dbus.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:tray_manager/tray_manager.dart' as tray;
import 'package:window_manager/window_manager.dart';

import '../state/hub_controller.dart';
import '../state/sender_controller.dart';

/// Asset of the tray icon.
const trayIconAsset = 'assets/tray_icon.png';

/// Whether this is a desktop platform with a tray and window control.
bool get isDesktopHost =>
    !kIsWeb && (Platform.isWindows || Platform.isMacOS || Platform.isLinux);

/// Prepares the window manager before `runApp` (desktop only).
///
/// The close button is intercepted only while [DesktopIntegration] is
/// mounted (it is the listener that handles the close), so the splash and
/// the startup error screen close normally.
Future<void> initDesktopWindow() async {
  if (!isDesktopHost) return;
  await windowManager.ensureInitialized();
}

/// How the tray icon of [os] (`Platform.operatingSystem`) reacts to clicks.
///
/// cnativeapi opens the context menu only for the configured trigger (the
/// default is none), and on Linux it exposes the menu over StatusNotifierItem
/// only with [tray.ContextMenuTrigger.clicked] (no click events arrive
/// there). So: Linux and macOS open the menu on click; Windows opens it on
/// right click and toggles the window on left click.
({tray.ContextMenuTrigger menuTrigger, bool clickTogglesWindow})
trayClickPolicy(String os) => switch (os) {
  'windows' => (
    menuTrigger: tray.ContextMenuTrigger.rightClicked,
    clickTogglesWindow: true,
  ),
  _ => (
    menuTrigger: tray.ContextMenuTrigger.clicked,
    clickTogglesWindow: false,
  ),
};

/// What closing the window does.
enum CloseAction {
  /// Hide the window; the tray icon brings it back.
  hideToTray,

  /// Stop sender and hub and quit.
  quit,
}

/// Closing hides to the tray only while something runs ([busy]) and a tray
/// icon is actually shown ([trayUsable]); otherwise the app quits, so the
/// window is never hidden without a way back.
CloseAction closeActionFor({required bool busy, required bool trayUsable}) =>
    busy && trayUsable ? CloseAction.hideToTray : CloseAction.quit;

/// Linux: whether a StatusNotifier host shows tray icons (KDE, or GNOME with
/// the AppIndicator extension). cnativeapi creates its icon whenever a session
/// bus exists, so this is asked before hiding the window into the tray.
/// Any D-Bus failure counts as "no host".
Future<bool> linuxTrayHostAvailable() async {
  DBusClient? client;
  try {
    client = DBusClient.session();
    for (final watcher in const [
      'org.kde.StatusNotifierWatcher',
      'com.canonical.StatusNotifierWatcher',
    ]) {
      if (!await client.nameHasOwner(watcher)) continue;
      final value = await DBusRemoteObject(
        client,
        name: watcher,
        path: DBusObjectPath('/StatusNotifierWatcher'),
      ).getProperty(watcher, 'IsStatusNotifierHostRegistered');
      if (value is DBusBoolean && value.value) return true;
    }
    return false;
  } catch (e) {
    debugPrint('tray host probe: $e');
    return false;
  } finally {
    unawaited(client?.close());
  }
}

/// Wraps the app with the tray icon and the close-to-tray behaviour.
/// With [enabled] false (mobile, tests) it only returns [child].
class DesktopIntegration extends ConsumerStatefulWidget {
  /// Creates the integration around [child].
  const DesktopIntegration({
    super.key,
    required this.child,
    required this.enabled,
    this.showTray = true,
    this.trayHostAvailable,
  });

  /// The app.
  final Widget child;

  /// Set up the tray and the window listener.
  final bool enabled;

  /// Create the tray icon (false in tests, which have no native tray).
  final bool showTray;

  /// Whether a created tray icon is actually visible; defaults to
  /// [linuxTrayHostAvailable] on Linux and `true` elsewhere.
  final Future<bool> Function()? trayHostAvailable;

  @override
  ConsumerState<DesktopIntegration> createState() => _DesktopIntegrationState();
}

class _DesktopIntegrationState extends ConsumerState<DesktopIntegration>
    with WindowListener {
  tray.TrayIcon? _trayIcon;
  tray.Menu? _menu;
  tray.MenuItem? _showItem;
  tray.MenuItem? _hubItem;
  final List<Object> _keepAlive = [];
  bool _quitting = false;

  @override
  void initState() {
    super.initState();
    if (!widget.enabled) return;
    windowManager.addListener(this);
    // Intercept the close button only while this listener handles it.
    unawaited(_setPreventClose(true));
    if (widget.showTray) _createTray();
  }

  @override
  void dispose() {
    if (widget.enabled) {
      windowManager.removeListener(this);
      _disposeTray();
      if (!_quitting) unawaited(_setPreventClose(false));
    }
    super.dispose();
  }

  static Future<void> _setPreventClose(bool prevent) async {
    try {
      await windowManager.setPreventClose(prevent);
    } catch (e) {
      debugPrint('window manager: $e');
    }
  }

  void _createTray() {
    try {
      final icon = tray.TrayIcon.create();
      final menu = tray.Menu.create();
      if (icon == null || menu == null) {
        debugPrint('tray: not available on this desktop');
        return;
      }
      final image = tray.ImageAsset.fromAsset(trayIconAsset);
      if (image != null) {
        icon.icon = image;
        _keepAlive.add(image);
      }
      icon.setTooltip('Headphone for All');

      final show = _item(
        'Hide window',
        tray.MenuItemType.normal,
        _toggleWindow,
      );
      final hub = _item('Hub', tray.MenuItemType.checkbox, _toggleHub);
      final quit = _item('Quit', tray.MenuItemType.normal, _quit);
      if (show == null || hub == null || quit == null) return;
      menu
        ..addItem(show)
        ..addItem(hub)
        ..addSeparator()
        ..addItem(quit);
      icon.setContextMenu(menu);
      final policy = trayClickPolicy(Platform.operatingSystem);
      // Without a trigger cnativeapi never opens (or, on Linux, exposes) it.
      icon.setContextMenuTrigger(policy.menuTrigger);
      if (policy.clickTogglesWindow) {
        icon.addListener((event) {
          if (event is tray.TrayIconClickedEvent) unawaited(_toggleWindow());
        });
      }
      icon.setVisible(true);
      _trayIcon = icon;
      _menu = menu;
      _showItem = show;
      _hubItem = hub;
      _syncHubItem(ref.read(hubControllerProvider).running);
      // The runner may start hidden (`--autostart`), so ask instead of assuming.
      unawaited(_syncShowItem());
    } catch (e) {
      // A missing tray host (e.g. GNOME without the AppIndicator extension)
      // must never break the app.
      debugPrint('tray: $e');
    }
  }

  tray.MenuItem? _item(
    String label,
    tray.MenuItemType type,
    FutureOr<void> Function() onClick,
  ) {
    final item = tray.MenuItem.createWithLabelAndType(label, type);
    item?.addListener((event) {
      if (event is tray.MenuItemClickedEvent) unawaited(Future(onClick));
    });
    return item;
  }

  void _disposeTray() {
    try {
      _trayIcon?.setVisible(false);
      _trayIcon?.dispose();
      _menu?.dispose();
      _showItem?.dispose();
      _hubItem?.dispose();
    } catch (e) {
      debugPrint('tray: $e');
    }
    _trayIcon = null;
    _keepAlive.clear();
  }

  void _syncHubItem(bool running) {
    final item = _hubItem;
    if (item == null) return;
    item
      ..label = running ? 'Hub is on' : 'Hub is off'
      ..state = running
          ? tray.MenuItemState.checked
          : tray.MenuItemState.unchecked;
  }

  /// Labels the Show/Hide item after the window's current visibility.
  Future<void> _syncShowItem() async {
    try {
      final visible = await windowManager.isVisible();
      if (mounted) _setVisible(visible);
    } catch (e) {
      debugPrint('window manager: $e');
    }
  }

  Future<void> _toggleWindow() async {
    if (await windowManager.isVisible()) {
      await _hideWindow();
    } else {
      await _showWindow();
    }
  }

  Future<void> _showWindow() async {
    await windowManager.show();
    await windowManager.focus();
    _setVisible(true);
  }

  Future<void> _hideWindow() async {
    await windowManager.hide();
    _setVisible(false);
  }

  Future<void> _toggleHub() async {
    await ref.read(hubControllerProvider.notifier).toggle();
    if (!mounted) return;
    // The window explains a failure (snack bar + hub screen); the tray
    // checkbox alone would just stay unchecked.
    if (ref.read(hubControllerProvider).error != null) {
      try {
        await _showWindow();
      } catch (e) {
        debugPrint('window manager: $e');
      }
    }
  }

  Future<void> _quit() async {
    if (_quitting) return;
    _quitting = true;
    try {
      await ref
          .read(senderControllerProvider.notifier)
          .stop()
          .timeout(const Duration(seconds: 3));
      await ref
          .read(hubControllerProvider.notifier)
          .stop()
          .timeout(const Duration(seconds: 3));
    } catch (e) {
      debugPrint('quit: $e');
    }
    _disposeTray();
    await windowManager.setPreventClose(false);
    await windowManager.destroy();
  }

  bool get _busy =>
      ref.read(hubControllerProvider).running ||
      ref.read(senderControllerProvider).isLive;

  Future<bool> _trayUsable() async {
    if (_trayIcon == null) return false;
    final probe =
        widget.trayHostAvailable ??
        (Platform.isLinux ? linuxTrayHostAvailable : null);
    if (probe == null) return true;
    return probe().timeout(const Duration(seconds: 2), onTimeout: () => false);
  }

  @override
  void onWindowClose() => unawaited(_onClose());

  Future<void> _onClose() async {
    final busy = _busy;
    final trayUsable = busy && await _trayUsable();
    if (!mounted) return;
    switch (closeActionFor(busy: busy, trayUsable: trayUsable)) {
      case CloseAction.hideToTray:
        await _hideWindow();
      case CloseAction.quit:
        await _quit();
    }
  }

  @override
  void onWindowFocus() => _setVisible(true);

  void _setVisible(bool visible) {
    _showItem?.label = visible ? 'Hide window' : 'Show window';
  }

  @override
  Widget build(BuildContext context) {
    if (widget.enabled) {
      ref.listen(
        hubControllerProvider.select((h) => h.running),
        (_, running) => _syncHubItem(running),
      );
    }
    return widget.child;
  }
}
