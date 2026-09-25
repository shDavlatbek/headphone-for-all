import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../state/core_providers.dart';
import '../state/hub_controller.dart';
import '../util/format.dart';
import '../widgets/source_tile.dart';
import 'pairing_sheet.dart';

/// The hub: start/stop, master volume, pairing and the live source mixer.
class HubScreen extends ConsumerWidget {
  /// Creates the screen.
  const HubScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final hub = ref.watch(hubControllerProvider);
    final controller = ref.read(hubControllerProvider.notifier);
    return ListView(
      padding: const EdgeInsets.all(16),
      children: [
        _HubHeader(hub: hub),
        const SizedBox(height: 16),
        if (hub.running) ...[
          Row(
            children: [
              Expanded(
                child: Text(
                  hub.sources.isEmpty
                      ? 'Sources'
                      : 'Sources (${hub.sources.length})',
                  style: Theme.of(context).textTheme.titleMedium,
                ),
              ),
              FilledButton.tonalIcon(
                key: const Key('pair-button'),
                icon: const Icon(Icons.qr_code_2),
                label: const Text('Pair a device'),
                onPressed: () => showPairingSheet(context, ref),
              ),
            ],
          ),
          const SizedBox(height: 8),
          if (hub.sources.isEmpty)
            const _EmptySources()
          else
            for (final source in hub.sources)
              SourceTile(
                key: ValueKey(source.streamId),
                source: source,
                onGainChanged: (g) => controller.setGain(source.streamId, g),
                onMutedChanged: (m) => controller.setMuted(source.streamId, m),
                onPriorityChanged: (p) =>
                    controller.setPriority(source.streamId, p),
              ),
        ],
      ],
    );
  }
}

class _HubHeader extends ConsumerWidget {
  const _HubHeader({required this.hub});

  final HubState hub;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final info = ref.watch(appInfoProvider);
    final controller = ref.read(hubControllerProvider.notifier);
    return Card(
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Icon(
                  hub.running ? Icons.headphones : Icons.headset_off_outlined,
                  size: 36,
                  color: hub.running
                      ? theme.colorScheme.primary
                      : theme.colorScheme.outline,
                ),
                const SizedBox(width: 16),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        hub.running ? 'Hub is on' : 'Hub is off',
                        key: const Key('hub-state'),
                        style: theme.textTheme.titleLarge,
                      ),
                      Text(
                        hub.running
                            ? '"${info.deviceName}" is visible on port '
                                  '${hub.port}. Audio plays on this device.'
                            : 'Turn it on to hear other devices here.',
                        style: theme.textTheme.bodyMedium,
                      ),
                    ],
                  ),
                ),
              ],
            ),
            if (hub.error != null) ...[
              const SizedBox(height: 12),
              _HubError(message: hub.error!, onDismiss: controller.clearError),
            ],
            const SizedBox(height: 12),
            Align(
              alignment: Alignment.centerRight,
              child: hub.busy
                  ? const Padding(
                      padding: EdgeInsets.all(8),
                      child: SizedBox.square(
                        dimension: 24,
                        child: CircularProgressIndicator(strokeWidth: 3),
                      ),
                    )
                  : hub.running
                  ? OutlinedButton.icon(
                      key: const Key('hub-toggle'),
                      icon: const Icon(Icons.stop),
                      label: const Text('Stop hub'),
                      onPressed: controller.stop,
                    )
                  : FilledButton.icon(
                      key: const Key('hub-toggle'),
                      icon: const Icon(Icons.play_arrow),
                      label: const Text('Start hub'),
                      onPressed: controller.start,
                    ),
            ),
            const Divider(height: 24),
            Row(
              children: [
                const Icon(Icons.volume_up_outlined),
                const SizedBox(width: 8),
                const Text('Master'),
                Expanded(
                  child: Slider(
                    key: const Key('master-gain'),
                    value: hub.masterGain.clamp(0.0, maxSliderGain),
                    max: maxSliderGain,
                    divisions: 40,
                    label: formatGain(hub.masterGain),
                    semanticFormatterCallback: formatGain,
                    onChanged: controller.previewMasterGain,
                    onChangeEnd: controller.setMasterGain,
                  ),
                ),
                SizedBox(
                  width: 48,
                  child: Text(
                    formatGain(hub.masterGain),
                    textAlign: TextAlign.end,
                  ),
                ),
              ],
            ),
          ],
        ),
      ),
    );
  }
}

/// The hub's last error, shown until dismissed (errors also appear in a
/// snack bar from the app shell, wherever the user is).
class _HubError extends StatelessWidget {
  const _HubError({required this.message, required this.onDismiss});

  final String message;
  final VoidCallback onDismiss;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    return Container(
      key: const Key('hub-error'),
      padding: const EdgeInsets.fromLTRB(12, 4, 4, 4),
      decoration: BoxDecoration(
        color: scheme.errorContainer,
        borderRadius: BorderRadius.circular(8),
      ),
      child: Row(
        children: [
          Icon(Icons.error_outline, color: scheme.onErrorContainer),
          const SizedBox(width: 12),
          Expanded(
            child: Text(
              message,
              style: TextStyle(color: scheme.onErrorContainer),
            ),
          ),
          IconButton(
            key: const Key('hub-error-dismiss'),
            tooltip: 'Dismiss',
            icon: Icon(Icons.close, color: scheme.onErrorContainer),
            onPressed: onDismiss,
          ),
        ],
      ),
    );
  }
}

class _EmptySources extends StatelessWidget {
  const _EmptySources();

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 32, horizontal: 16),
      child: Column(
        children: [
          Icon(
            Icons.speaker_group_outlined,
            size: 48,
            color: theme.colorScheme.outline,
          ),
          const SizedBox(height: 12),
          Text('No device is sending yet.', style: theme.textTheme.titleMedium),
          const SizedBox(height: 4),
          const Text(
            'On another device, open Headphone for All, choose Send and pick '
            'this hub. New devices need to pair once.',
            textAlign: TextAlign.center,
          ),
        ],
      ),
    );
  }
}
