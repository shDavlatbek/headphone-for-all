/// Links to the project's documentation, and opening them in the browser.
library;

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:url_launcher/url_launcher.dart' as launcher;

import '../widgets/dialogs.dart';

/// Project home page.
const projectUrl = 'https://github.com/shDavlatbek/headphone-for-all';

/// Where to report problems.
const issuesUrl = '$projectUrl/issues';

/// The user guide (docs/USER_GUIDE.md).
const userGuideUrl = '$projectUrl/blob/main/docs/USER_GUIDE.md';

/// Which Android apps can be captured (docs/ANDROID_APPS.md).
const androidAppsUrl = '$projectUrl/blob/main/docs/ANDROID_APPS.md';

/// Opens a link outside the app; `true` when a browser (or another app)
/// took it.
typedef LinkOpener = Future<bool> Function(Uri uri);

/// How links are opened: the platform browser through url_launcher
/// (overridden in tests).
final linkOpenerProvider = Provider<LinkOpener>(
  (ref) =>
      (uri) => launcher.launchUrl(
        uri,
        mode: launcher.LaunchMode.externalApplication,
      ),
);

/// Opens [url] in the browser. Where that fails (no browser, a sandbox that
/// refuses), the link is copied instead and a snack bar says so.
Future<void> openLink(BuildContext context, WidgetRef ref, String url) async {
  var opened = false;
  try {
    opened = await ref.read(linkOpenerProvider)(Uri.parse(url));
  } catch (e) {
    debugPrint('open $url: $e');
  }
  if (opened) return;
  await Clipboard.setData(ClipboardData(text: url));
  if (context.mounted) {
    showMessage(context, 'Could not open a browser. Link copied: $url');
  }
}
