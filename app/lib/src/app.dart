import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import 'screens/about_screen.dart';
import 'screens/home_screen.dart';
import 'screens/hub_screen.dart';
import 'screens/sender_screen.dart';
import 'screens/settings_screen.dart';
import 'state/app_prefs.dart';
import 'state/core_providers.dart';
import 'state/hub_controller.dart';
import 'state/navigation.dart';
import 'state/sender_controller.dart';
import 'widgets/dialogs.dart';

/// Title shown in the app bar, window and task switcher.
const appTitle = 'Headphone for All';

/// Seed of the Material 3 colour scheme.
const seedColor = Color(0xFF3F51B5);

/// Width from which the shell uses a [NavigationRail] instead of a
/// [NavigationBar].
const railBreakpoint = 640.0;

/// The app's light theme.
ThemeData lightTheme() => ThemeData(
  colorScheme: ColorScheme.fromSeed(seedColor: seedColor),
  useMaterial3: true,
);

/// The app's dark theme.
ThemeData darkTheme() => ThemeData(
  colorScheme: ColorScheme.fromSeed(
    seedColor: seedColor,
    brightness: Brightness.dark,
  ),
  useMaterial3: true,
);

/// Root widget (expects a `ProviderScope` with the core providers above it).
class HfaApp extends StatelessWidget {
  /// Creates the app.
  const HfaApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: appTitle,
      debugShowCheckedModeBanner: false,
      theme: lightTheme(),
      darkTheme: darkTheme(),
      home: const AppShell(),
    );
  }
}

/// One navigation destination.
typedef _Destination = ({
  AppSection section,
  String label,
  IconData icon,
  IconData selectedIcon,
});

const List<_Destination> _destinations = [
  (
    section: AppSection.home,
    label: 'Home',
    icon: Icons.home_outlined,
    selectedIcon: Icons.home,
  ),
  (
    section: AppSection.hub,
    label: 'Hub',
    icon: Icons.headphones_outlined,
    selectedIcon: Icons.headphones,
  ),
  (
    section: AppSection.sender,
    label: 'Send',
    icon: Icons.podcasts_outlined,
    selectedIcon: Icons.podcasts,
  ),
  (
    section: AppSection.settings,
    label: 'Settings',
    icon: Icons.settings_outlined,
    selectedIcon: Icons.settings,
  ),
  (
    section: AppSection.about,
    label: 'About',
    icon: Icons.info_outline,
    selectedIcon: Icons.info,
  ),
];

/// Adaptive scaffold: [NavigationRail] on wide windows, [NavigationBar] on
/// phones.
class AppShell extends ConsumerWidget {
  /// Creates the shell.
  const AppShell({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final section = ref.watch(sectionProvider);
    final index = _destinations.indexWhere((d) => d.section == section);
    final demo = ref.watch(demoModeProvider);
    final hubOn = ref.watch(hubControllerProvider.select((h) => h.running));
    final sending = ref.watch(senderControllerProvider.select((s) => s.isLive));
    final select = ref.read(sectionProvider.notifier).select;
    // Hub errors (a failed start from the tray, a lost output device...)
    // are announced wherever the user is; the hub screen keeps the last one.
    ref.listen(hubControllerProvider.select((h) => h.error), (_, error) {
      if (error != null) showMessage(context, error);
    });
    // Loaded at launch: it starts the hub if the user asked for that.
    ref.listen(appPrefsProvider, (_, _) {});

    Widget badged(IconData icon, AppSection s) {
      final on =
          (s == AppSection.hub && hubOn) || (s == AppSection.sender && sending);
      return Badge(isLabelVisible: on, smallSize: 8, child: Icon(icon));
    }

    final body = switch (section) {
      AppSection.home => const HomeScreen(),
      AppSection.hub => const HubScreen(),
      AppSection.sender => const SenderScreen(),
      AppSection.settings => const SettingsScreen(),
      AppSection.about => const AboutScreen(),
    };

    return LayoutBuilder(
      builder: (context, constraints) {
        final wide = constraints.maxWidth >= railBreakpoint;
        final content = Center(
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 900),
            child: KeyedSubtree(key: ValueKey(section), child: body),
          ),
        );
        return Scaffold(
          appBar: AppBar(
            title: Text(index == 0 ? appTitle : _destinations[index].label),
            actions: [
              if (demo)
                const Padding(
                  padding: EdgeInsets.only(right: 12),
                  child: Chip(label: Text('Demo')),
                ),
            ],
          ),
          body: wide
              ? Row(
                  children: [
                    SafeArea(
                      right: false,
                      child: NavigationRail(
                        selectedIndex: index,
                        labelType: NavigationRailLabelType.all,
                        onDestinationSelected: (i) =>
                            select(_destinations[i].section),
                        destinations: [
                          for (final d in _destinations)
                            NavigationRailDestination(
                              icon: badged(d.icon, d.section),
                              selectedIcon: badged(d.selectedIcon, d.section),
                              label: Text(d.label),
                            ),
                        ],
                      ),
                    ),
                    const VerticalDivider(width: 1),
                    Expanded(child: content),
                  ],
                )
              : content,
          bottomNavigationBar: wide
              ? null
              : NavigationBar(
                  selectedIndex: index,
                  onDestinationSelected: (i) =>
                      select(_destinations[i].section),
                  destinations: [
                    for (final d in _destinations)
                      NavigationDestination(
                        icon: badged(d.icon, d.section),
                        selectedIcon: badged(d.selectedIcon, d.section),
                        label: d.label,
                      ),
                  ],
                ),
        );
      },
    );
  }
}
