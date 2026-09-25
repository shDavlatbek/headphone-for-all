import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../state/core_providers.dart';
import '../util/format.dart';
import '../widgets/dialogs.dart';

/// Project home page.
const projectUrl = 'https://github.com/shDavlatbek/headphone-for-all';

/// Where to report problems.
const issuesUrl = '$projectUrl/issues';

/// Version, device identity, licences and links.
class AboutScreen extends ConsumerWidget {
  /// Creates the screen.
  const AboutScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final info = ref.watch(appInfoProvider);
    final demo = ref.watch(demoModeProvider);
    return ListView(
      padding: const EdgeInsets.all(16),
      children: [
        Card(
          child: Padding(
            padding: const EdgeInsets.all(16),
            child: Row(
              children: [
                Icon(
                  Icons.headphones,
                  size: 48,
                  color: theme.colorScheme.primary,
                ),
                const SizedBox(width: 16),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        'Headphone for All',
                        style: theme.textTheme.titleLarge,
                      ),
                      Text(
                        'Version ${info.version}${demo ? ' · demo mode' : ''}',
                        key: const Key('about-version'),
                      ),
                      const SizedBox(height: 4),
                      const Text(
                        'Audio from all your devices, at the same time, in '
                        'one headphone. Everything stays on your local '
                        'network and is end-to-end encrypted.',
                      ),
                    ],
                  ),
                ),
              ],
            ),
          ),
        ),
        const SizedBox(height: 12),
        Card(
          child: Column(
            children: [
              ListTile(
                leading: const Icon(Icons.fingerprint),
                title: const Text('Device id'),
                subtitle: SelectableText(info.deviceId),
              ),
              ListTile(
                leading: Icon(platformIcon(info.platform)),
                title: const Text('Platform'),
                subtitle: Text(platformName(info.platform)),
              ),
              _LinkTile(
                icon: Icons.code,
                title: 'Source code',
                url: projectUrl,
              ),
              _LinkTile(
                icon: Icons.bug_report_outlined,
                title: 'Report a problem',
                url: issuesUrl,
              ),
              ListTile(
                key: const Key('licences'),
                leading: const Icon(Icons.description_outlined),
                title: const Text('Open-source licences'),
                subtitle: const Text('MIT OR Apache-2.0, and third parties'),
                onTap: () => showLicensePage(
                  context: context,
                  applicationName: 'Headphone for All',
                  applicationVersion: info.version,
                  applicationIcon: const Padding(
                    padding: EdgeInsets.all(8),
                    child: Icon(Icons.headphones, size: 48),
                  ),
                ),
              ),
            ],
          ),
        ),
      ],
    );
  }
}

/// A link: tapping copies it (the app ships no browser launcher).
class _LinkTile extends StatelessWidget {
  const _LinkTile({required this.icon, required this.title, required this.url});

  final IconData icon;
  final String title;
  final String url;

  @override
  Widget build(BuildContext context) {
    return ListTile(
      leading: Icon(icon),
      title: Text(title),
      subtitle: Text(url),
      trailing: const Icon(Icons.copy, size: 18),
      onTap: () async {
        await Clipboard.setData(ClipboardData(text: url));
        if (context.mounted) showMessage(context, 'Link copied');
      },
    );
  }
}
