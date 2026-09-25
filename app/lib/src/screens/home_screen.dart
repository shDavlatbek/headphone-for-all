import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/hfa_api.dart';
import '../state/core_providers.dart';
import '../state/hub_controller.dart';
import '../state/navigation.dart';
import '../state/sender_controller.dart';
import '../util/format.dart';

/// Role choice ("Hub" or "Sender"), this device's name and what it can do.
class HomeScreen extends ConsumerWidget {
  /// Creates the screen.
  const HomeScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final info = ref.watch(appInfoProvider);
    final hubRunning = ref.watch(
      hubControllerProvider.select((h) => h.running),
    );
    final senderState = ref.watch(
      senderControllerProvider.select((s) => s.status.state),
    );
    final section = ref.read(sectionProvider.notifier);
    return ListView(
      padding: const EdgeInsets.all(16),
      children: [
        Text(
          'Hear every device in one headphone',
          style: theme.textTheme.headlineSmall,
        ),
        const SizedBox(height: 4),
        Text(
          'Pick what this device does. You can change it any time.',
          style: theme.textTheme.bodyMedium,
        ),
        const SizedBox(height: 16),
        LayoutBuilder(
          builder: (context, constraints) {
            final cards = [
              _RoleCard(
                key: const Key('role-hub'),
                icon: Icons.headphones,
                title: 'Headphone is connected here',
                role: 'Hub',
                description:
                    'Receive audio from your other devices and mix it with '
                    "this device's own sound.",
                status: hubRunning ? 'Hub is on' : null,
                onTap: () => section.select(AppSection.hub),
              ),
              _RoleCard(
                key: const Key('role-sender'),
                icon: Icons.podcasts,
                title: "Send this device's audio",
                role: 'Sender',
                description:
                    'Stream what this device plays to the hub your '
                    'headphone is connected to.',
                status: liveSenderStates.contains(senderState)
                    ? senderStateLabel(senderState)
                    : null,
                onTap: () => section.select(AppSection.sender),
              ),
            ];
            if (constraints.maxWidth < 600) {
              return Column(
                children: [cards[0], const SizedBox(height: 12), cards[1]],
              );
            }
            return IntrinsicHeight(
              child: Row(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  Expanded(child: cards[0]),
                  const SizedBox(width: 12),
                  Expanded(child: cards[1]),
                ],
              ),
            );
          },
        ),
        const SizedBox(height: 16),
        _DeviceCard(info: info),
      ],
    );
  }
}

class _RoleCard extends StatelessWidget {
  const _RoleCard({
    super.key,
    required this.icon,
    required this.title,
    required this.role,
    required this.description,
    required this.status,
    required this.onTap,
  });

  final IconData icon;
  final String title;
  final String role;
  final String description;
  final String? status;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final status = this.status;
    return Card(
      clipBehavior: Clip.antiAlias,
      color: scheme.secondaryContainer,
      child: InkWell(
        onTap: onTap,
        child: Padding(
          padding: const EdgeInsets.all(20),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  Icon(icon, size: 40, color: scheme.onSecondaryContainer),
                  const Spacer(),
                  if (status != null)
                    Chip(
                      avatar: const Icon(Icons.circle, size: 10),
                      label: Text(status),
                    ),
                ],
              ),
              const SizedBox(height: 12),
              Text(
                role,
                style: theme.textTheme.labelLarge?.copyWith(
                  color: scheme.onSecondaryContainer,
                ),
              ),
              Text(
                title,
                style: theme.textTheme.titleLarge?.copyWith(
                  color: scheme.onSecondaryContainer,
                ),
              ),
              const SizedBox(height: 8),
              Text(
                description,
                style: theme.textTheme.bodyMedium?.copyWith(
                  color: scheme.onSecondaryContainer,
                ),
              ),
            ],
          ),
        ),
      ),
    );
  }
}

class _DeviceCard extends ConsumerWidget {
  const _DeviceCard({required this.info});

  final AppInfo info;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final caps = info.capabilities;
    final notes = capabilityNotes(info);
    return Card(
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 8),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            ListTile(
              leading: Icon(platformIcon(info.platform)),
              title: Text(info.deviceName, key: const Key('device-name-label')),
              subtitle: Text(
                '${platformName(info.platform)} · ${info.deviceId}',
              ),
              trailing: TextButton(
                onPressed: () => ref
                    .read(sectionProvider.notifier)
                    .select(AppSection.settings),
                child: const Text('Rename'),
              ),
            ),
            const Divider(),
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 4, 16, 4),
              child: Text(
                'What this device can send',
                style: theme.textTheme.titleSmall,
              ),
            ),
            for (final note in notes)
              ListTile(
                dense: true,
                leading: Icon(
                  note.ok ? Icons.check_circle_outline : Icons.info_outline,
                  color: note.ok
                      ? theme.colorScheme.primary
                      : theme.colorScheme.onSurfaceVariant,
                ),
                title: Text(note.text),
              ),
            if (caps.notes.isNotEmpty)
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 4, 16, 8),
                child: Text(
                  caps.notes,
                  key: const Key('capability-notes'),
                  style: theme.textTheme.bodySmall,
                ),
              ),
          ],
        ),
      ),
    );
  }
}

/// One line of the capability summary.
typedef CapabilityNote = ({bool ok, String text});

/// Plain-language summary of what [info]'s platform can capture.
List<CapabilityNote> capabilityNotes(AppInfo info) {
  final caps = info.capabilities;
  return [
    if (info.platform == 'android')
      (
        ok: true,
        text:
            'Playback of other apps (Android 10+, apps may opt out; calls '
            'are never captured)',
      ),
    if (info.platform == 'ios')
      (
        ok: true,
        text:
            'Other apps through a screen broadcast (protected/DRM audio is '
            'silent)',
      ),
    if (caps.systemMix) (ok: true, text: 'Everything this device plays'),
    if (caps.perApp) (ok: true, text: 'Single apps'),
    if (!caps.systemMix && !caps.externalOnly)
      (ok: false, text: 'No system capture on this platform (test tone only)'),
    if (caps.mutesLocalOutput)
      (
        ok: false,
        text: "Captured audio is muted on this device's own speakers",
      ),
    (ok: true, text: 'Receiving as a hub'),
  ];
}
