import 'package:flutter_riverpod/flutter_riverpod.dart';

/// The top-level sections of the app, in navigation order.
enum AppSection {
  /// Role choice and device overview.
  home,

  /// Hub: incoming sources and mixer.
  hub,

  /// Sender: stream this device's audio.
  sender,

  /// Settings and trusted devices.
  settings,

  /// Version and licences.
  about,
}

/// The section shown by the app shell.
final sectionProvider = NotifierProvider<SectionNotifier, AppSection>(
  SectionNotifier.new,
);

/// Holds the selected [AppSection].
class SectionNotifier extends Notifier<AppSection> {
  @override
  AppSection build() => AppSection.home;

  /// Shows [section].
  void select(AppSection section) => state = section;
}
