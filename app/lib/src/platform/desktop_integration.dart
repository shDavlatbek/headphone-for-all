/// Desktop (Windows, macOS, Linux) shell integration: a tray icon with
/// Show/Hide, Hub on/off and Quit, and "close hides to the tray" while the
/// hub or the sender runs.
library;

import 'dart:async';
import 'dart:io';

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

/// Prepares the window before `runApp` (desktop only): intercepts the close
/// button so [DesktopIntegration] can hide to the tray instead.
Future<void> initDesktopWindow() async {
  if (!isDesktopHost) return;
  await windowManager.ensureInitialized();
  await windowManager.setPreventClose(true);
}

/// Wraps the app with the tray icon and the close-to-tray behaviour.
/// With [enabled] false (mobile, tests) it only returns [child].
class DesktopIntegration extends ConsumerStatefulWidget {
  /// Creates the integration around [child].
  const DesktopIntegration({
    super.key,
    required this.child,
    required this.enabled,
  });

  /// The app.
  final Widget child;

  /// Set up the tray and the window listener.
  final bool enabled;

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
    _createTray();
  }

  @override
  void dispose() {
    if (widget.enabled) {
      windowManager.removeListener(this);
      _disposeTray();
    }
    super.dispose();
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
      if (!Platform.isMacOS) {
        // Windows/Linux: left click toggles the window, right click = menu.
        icon.addListener((event) {
          if (event is tray.TrayIconClickedEvent) _toggleWindow();
        });
      }
      icon.setVisible(true);
      _trayIcon = icon;
      _menu = menu;
      _showItem = show;
      _hubItem = hub;
      _syncHubItem(ref.read(hubControllerProvider).running);
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

  Future<void> _toggleWindow() async {
    if (await windowManager.isVisible()) {
      await _hideWindow();
    } else {
      await windowManager.show();
      await windowManager.focus();
      _setVisible(true);
    }
  }

  Future<void> _hideWindow() async {
    await windowManager.hide();
    _setVisible(false);
  }

  Future<void> _toggleHub() =>
      ref.read(hubControllerProvider.notifier).toggle();

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

  @override
  void onWindowClose() {
    if (_busy && _trayIcon != null) {
      unawaited(_hideWindow());
    } else {
      unawaited(_quit());
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
