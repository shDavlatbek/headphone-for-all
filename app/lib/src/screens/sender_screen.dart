import 'package:flutter/material.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/hfa_api.dart';
import '../models/hub_target.dart';
import '../models/source_choice.dart';
import '../state/core_providers.dart';
import '../state/discovery_controller.dart';
import '../state/sender_controller.dart';
import '../state/settings_controller.dart';
import '../util/format.dart';
import '../widgets/broadcast_picker.dart';
import '../widgets/dialogs.dart';
import '../widgets/level_meter.dart';
import 'qr_scan_screen.dart';

/// The sender: pick a hub and a source, start/stop, live status.
class SenderScreen extends ConsumerWidget {
  /// Creates the screen.
  const SenderScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final sender = ref.watch(senderControllerProvider);
    return ListView(
      padding: const EdgeInsets.all(16),
      children: [
        const SenderStatusCard(),
        const SizedBox(height: 16),
        const _HubPicker(),
        const SizedBox(height: 16),
        _SourcePicker(enabled: !sender.isLive && !sender.busy),
      ],
    );
  }
}

/// Starts the sender, asking for a PIN first when the hub needs one.
Future<void> startSending(BuildContext context, WidgetRef ref) async {
  final sender = ref.read(senderControllerProvider);
  final target = sender.target;
  if (target == null) return;
  String? pin;
  if (target.needsPin) {
    pin = await showPinDialog(context, hubName: target.name);
    if (pin == null) return;
  }
  await ref.read(senderControllerProvider.notifier).start(pin: pin);
}

/// Asks for a PIN and retries after a "pairing required" failure.
Future<void> retryWithPin(BuildContext context, WidgetRef ref) async {
  final target = ref.read(senderControllerProvider).target;
  if (target == null) return;
  final pin = await showPinDialog(context, hubName: target.name);
  if (pin == null) return;
  await ref.read(senderControllerProvider.notifier).start(pin: pin);
}

/// State, hub, RTT, loss, bitrate and level, plus Start/Stop.
class SenderStatusCard extends ConsumerWidget {
  /// Creates the card.
  const SenderStatusCard({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final sender = ref.watch(senderControllerProvider);
    final info = ref.watch(appInfoProvider);
    final controller = ref.read(senderControllerProvider.notifier);
    final status = sender.status;
    final isBroadcast = sender.source?.kind == SourceKind.broadcast;
    final streaming = status.state == 'streaming';
    final error = sender.error ?? status.error;
    final canStart =
        sender.target != null &&
        (sender.source?.isComplete ?? false) &&
        !sender.busy;

    final Widget action;
    if (sender.busy) {
      action = const SizedBox.square(
        dimension: 24,
        child: CircularProgressIndicator(strokeWidth: 3),
      );
    } else if (sender.isLive) {
      action = OutlinedButton.icon(
        key: const Key('sender-stop'),
        icon: const Icon(Icons.stop),
        label: const Text('Stop'),
        onPressed: controller.stop,
      );
    } else {
      action = FilledButton.icon(
        key: const Key('sender-start'),
        icon: Icon(
          isBroadcast ? Icons.settings_input_antenna : Icons.play_arrow,
        ),
        label: Text(isBroadcast ? 'Prepare broadcast' : 'Start sending'),
        onPressed: canStart ? () => startSending(context, ref) : null,
      );
    }

    return Card(
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Icon(
                  streaming || sender.broadcasting
                      ? Icons.podcasts
                      : Icons.podcasts_outlined,
                  size: 36,
                  color: streaming || sender.broadcasting
                      ? theme.colorScheme.primary
                      : theme.colorScheme.outline,
                ),
                const SizedBox(width: 16),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        sender.broadcasting
                            ? 'Broadcasting'
                            : senderStateLabel(status.state),
                        key: const Key('sender-state'),
                        style: theme.textTheme.titleLarge,
                      ),
                      Text(
                        _subtitle(sender),
                        style: theme.textTheme.bodyMedium,
                      ),
                    ],
                  ),
                ),
              ],
            ),
            const SizedBox(height: 12),
            Align(alignment: Alignment.centerRight, child: action),
            if (sender.isLive) ...[
              const SizedBox(height: 12),
              LevelMeter(levelDb: status.levelDb),
              const SizedBox(height: 8),
              Wrap(
                spacing: 16,
                runSpacing: 4,
                children: [
                  Text('RTT ${formatMs(status.rttMs)}'),
                  Text('loss ${formatPercent(status.lossPct)}'),
                  Text(formatBitrate(status.bitrate)),
                  Text('level ${formatDb(status.levelDb)}'),
                ],
              ),
              // A live sender's error is a warning: e.g. the capture fell
              // back to the whole system mix (§8.5), or why it reconnects.
              if (error != null) ...[
                const SizedBox(height: 8),
                Row(
                  key: const Key('sender-warning'),
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Icon(
                      Icons.warning_amber_rounded,
                      size: 20,
                      color: theme.colorScheme.tertiary,
                    ),
                    const SizedBox(width: 8),
                    Expanded(
                      child: Text(
                        error,
                        style: theme.textTheme.bodyMedium?.copyWith(
                          color: theme.colorScheme.onSurfaceVariant,
                        ),
                      ),
                    ),
                  ],
                ),
              ],
            ],
            if (error != null && !sender.isLive) ...[
              const SizedBox(height: 12),
              Text(
                error,
                key: const Key('sender-error'),
                style: TextStyle(color: theme.colorScheme.error),
              ),
              if (sender.needsPin && sender.target != null)
                Align(
                  alignment: Alignment.centerLeft,
                  child: TextButton.icon(
                    key: const Key('enter-pin'),
                    icon: const Icon(Icons.pin_outlined),
                    label: const Text('Enter PIN'),
                    onPressed: () => retryWithPin(context, ref),
                  ),
                ),
            ],
            if (isBroadcast && sender.broadcastReady) ...[
              const Divider(height: 24),
              Row(
                children: [
                  const BroadcastPicker(),
                  const SizedBox(width: 16),
                  Expanded(
                    child: Text(
                      // The extension connects in the background and does
                      // not report the connection (§8.9), so this does not
                      // claim that audio arrives.
                      sender.broadcasting
                          ? 'The broadcast to '
                                '${sender.target?.name ?? 'the hub'} has '
                                'started. Check on the hub that the audio '
                                'arrives. Tap the button to stop it.'
                          : 'Tap the button, choose "Headphone for All" and '
                                'Start Broadcast. Then switch to the app you '
                                'want to hear.',
                    ),
                  ),
                ],
              ),
            ],
            if (info.isAndroid &&
                sender.source?.kind == SourceKind.deviceAudio &&
                !sender.isLive) ...[
              const SizedBox(height: 8),
              Text(
                'Android will ask to allow capturing audio. A notification '
                'stays visible while sending.',
                style: theme.textTheme.bodySmall,
              ),
            ],
          ],
        ),
      ),
    );
  }

  static String _subtitle(SenderState sender) {
    final target = sender.target;
    final hubName = sender.status.hubName ?? target?.name;
    if (sender.isLive) return 'To ${hubName ?? 'the hub'}';
    if (target == null) return 'Choose a hub below.';
    if (sender.source?.kind == SourceKind.broadcast && sender.broadcastReady) {
      return 'Ready to broadcast to ${target.name}';
    }
    return 'Hub: ${target.name}';
  }
}

class _HubPicker extends ConsumerWidget {
  const _HubPicker();

  Future<void> _usePairingUri(
    BuildContext context,
    WidgetRef ref,
    String uri,
  ) async {
    try {
      final parsed = await ref.read(hfaApiProvider).parsePairingUri(uri);
      ref
          .read(senderControllerProvider.notifier)
          .selectTarget(HubTarget.fromPairingUri(parsed));
    } catch (e) {
      if (context.mounted) {
        showMessage(context, 'Not a valid pairing code: ${describeError(e)}');
      }
    }
  }

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final info = ref.watch(appInfoProvider);
    final discovery = ref.watch(discoveryControllerProvider);
    final peers = ref.watch(trustedPeersProvider).value ?? const [];
    final sender = ref.watch(senderControllerProvider);
    final controller = ref.read(senderControllerProvider.notifier);
    final locked = sender.isLive || sender.busy;
    final selected = sender.target;

    final peerIds = {for (final p in peers) p.deviceId};
    final discovered = [
      for (final hub in discovery.hubs.values)
        if (hub.deviceId != info.deviceId)
          HubTarget.discovered(
            hub,
            trusted: hub.trusted || peerIds.contains(hub.deviceId),
          ),
    ];
    final seen = {for (final t in discovered) t.deviceId};
    final paired = [
      for (final peer in peers)
        if (!seen.contains(peer.deviceId) && peer.deviceId != info.deviceId)
          HubTarget.paired(peer),
    ];
    final extra =
        selected != null &&
            !discovered.any((t) => t.key == selected.key) &&
            !paired.any((t) => t.key == selected.key)
        ? selected
        : null;

    Widget tile(HubTarget target) {
      final isSelected = selected?.key == target.key;
      return ListTile(
        key: Key('hub-${target.key}'),
        enabled: !locked,
        selected: isSelected,
        leading: Icon(
          target.platform.isEmpty
              ? Icons.headphones_outlined
              : platformIcon(target.platform),
        ),
        title: Text(target.name),
        subtitle: Text(_hubSubtitle(target)),
        trailing: isSelected
            ? const Icon(Icons.check_circle)
            : const Icon(Icons.radio_button_unchecked),
        onTap: () => controller.selectTarget(target),
      );
    }

    return Card(
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 8),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 8, 8, 0),
              child: Row(
                children: [
                  Expanded(
                    child: Text('Hub', style: theme.textTheme.titleMedium),
                  ),
                  IconButton(
                    tooltip: 'Search again',
                    icon: const Icon(Icons.refresh),
                    onPressed: locked
                        ? null
                        : ref
                              .read(discoveryControllerProvider.notifier)
                              .refresh,
                  ),
                ],
              ),
            ),
            if (discovered.isEmpty && paired.isEmpty && extra == null)
              Padding(
                padding: const EdgeInsets.all(16),
                child: Row(
                  children: [
                    Icon(
                      discovery.error == null
                          ? Icons.wifi_find
                          : Icons.wifi_off_outlined,
                      color: theme.colorScheme.onSurfaceVariant,
                    ),
                    const SizedBox(width: 12),
                    Expanded(
                      child: Text(
                        discovery.error ??
                            'Looking for hubs on this network… Start the hub '
                                'on the device your headphone is connected to.',
                      ),
                    ),
                  ],
                ),
              ),
            if (extra != null) tile(extra),
            ...discovered.map(tile),
            if (paired.isNotEmpty) ...[
              Padding(
                padding: const EdgeInsets.fromLTRB(16, 12, 16, 4),
                child: Text(
                  'Paired, not seen right now',
                  style: theme.textTheme.labelLarge,
                ),
              ),
              ...paired.map(tile),
            ],
            Padding(
              padding: const EdgeInsets.fromLTRB(8, 8, 8, 0),
              child: Wrap(
                spacing: 8,
                children: [
                  TextButton.icon(
                    key: const Key('add-by-address'),
                    icon: const Icon(Icons.add_link),
                    label: const Text('Add by address'),
                    onPressed: locked
                        ? null
                        : () async {
                            final target = await showAddHubDialog(context);
                            if (target != null) controller.selectTarget(target);
                          },
                  ),
                  if (info.isMobile)
                    TextButton.icon(
                      key: const Key('scan-qr'),
                      icon: const Icon(Icons.qr_code_scanner),
                      label: const Text('Scan QR'),
                      onPressed: locked
                          ? null
                          : () async {
                              final uri = await scanPairingQr(context);
                              if (uri != null && context.mounted) {
                                await _usePairingUri(context, ref, uri);
                              }
                            },
                    )
                  else
                    TextButton.icon(
                      key: const Key('pairing-link'),
                      icon: const Icon(Icons.link),
                      label: const Text('Pairing link'),
                      onPressed: locked
                          ? null
                          : () async {
                              final uri = await showPairingLinkDialog(context);
                              if (uri != null && context.mounted) {
                                await _usePairingUri(context, ref, uri);
                              }
                            },
                    ),
                  TextButton.icon(
                    key: const Key('enter-pin-button'),
                    icon: const Icon(Icons.pin_outlined),
                    label: const Text('Enter PIN'),
                    onPressed: locked || selected == null
                        ? null
                        : () async {
                            final pin = await showPinDialog(
                              context,
                              hubName: selected.name,
                            );
                            if (pin != null) {
                              controller.selectTarget(selected.withPin(pin));
                            }
                          },
                  ),
                ],
              ),
            ),
          ],
        ),
      ),
    );
  }

  static String _hubSubtitle(HubTarget target) {
    final parts = <String>[target.address];
    if (target.pairingSecret != null) {
      parts.add(
        target.origin == HubOrigin.pairingLink ? 'from QR code' : 'PIN entered',
      );
    } else if (target.trusted) {
      parts.add('paired');
    } else if (target.origin == HubOrigin.discovered) {
      parts.add('needs PIN');
    }
    return parts.join(' · ');
  }
}

class _SourcePicker extends ConsumerWidget {
  const _SourcePicker({required this.enabled});

  final bool enabled;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final info = ref.watch(appInfoProvider);
    final sender = ref.watch(senderControllerProvider);
    final controller = ref.read(senderControllerProvider.notifier);
    final kinds = availableSources(info);
    final selected = sender.source;
    final caps = info.capabilities;

    return Card(
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 8),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 8, 16, 0),
              child: Text('Source', style: theme.textTheme.titleMedium),
            ),
            RadioGroup<SourceKind>(
              groupValue: selected?.kind,
              onChanged: (kind) {
                if (kind == null || !enabled) return;
                controller.selectSource(SourceChoice(kind));
              },
              child: Column(
                children: [
                  for (final kind in kinds)
                    RadioListTile<SourceKind>(
                      key: Key('source-${kind.name}'),
                      value: kind,
                      enabled: enabled,
                      title: Text(sourceTitle(kind)),
                      subtitle: Text(sourceSubtitle(kind)),
                    ),
                ],
              ),
            ),
            if (selected?.kind == SourceKind.app)
              _AppChooser(selected: selected?.app, enabled: enabled),
            if (caps.mutesLocalOutput &&
                (selected?.kind == SourceKind.system ||
                    selected?.kind == SourceKind.systemExceptThisApp ||
                    selected?.kind == SourceKind.app))
              _Note(
                icon: Icons.volume_off_outlined,
                text:
                    'While sending, the captured audio is muted on this '
                    "device's own speakers.",
              ),
            if (caps.notes.isNotEmpty)
              _Note(icon: Icons.info_outline, text: caps.notes),
          ],
        ),
      ),
    );
  }
}

class _AppChooser extends ConsumerWidget {
  const _AppChooser({required this.selected, required this.enabled});

  final CaptureAppDto? selected;
  final bool enabled;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final apps = ref.watch(captureAppsProvider);
    final controller = ref.read(senderControllerProvider.notifier);
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 0, 8, 8),
      child: apps.when(
        loading: () => const LinearProgressIndicator(),
        error: (e, _) => Text('Could not list apps: ${describeError(e)}'),
        data: (list) {
          final current = list.contains(selected) ? selected : null;
          return Row(
            children: [
              Expanded(
                child: list.isEmpty
                    ? const Text(
                        'No app is playing audio right now. Start playback, '
                        'then refresh.',
                      )
                    : DropdownButton<CaptureAppDto>(
                        key: const Key('app-dropdown'),
                        isExpanded: true,
                        value: current,
                        hint: const Text('Choose an app'),
                        items: [
                          for (final app in list)
                            DropdownMenuItem(
                              value: app,
                              child: Text('${app.name} (${app.pid})'),
                            ),
                        ],
                        onChanged: enabled
                            ? (app) => controller.selectSource(
                                SourceChoice(SourceKind.app, app: app),
                              )
                            : null,
                      ),
              ),
              IconButton(
                tooltip: 'Refresh apps',
                icon: const Icon(Icons.refresh),
                onPressed: () => ref.invalidate(captureAppsProvider),
              ),
            ],
          );
        },
      ),
    );
  }
}

class _Note extends StatelessWidget {
  const _Note({required this.icon, required this.text});

  final IconData icon;
  final String text;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.fromLTRB(16, 8, 16, 8),
      child: Row(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Icon(icon, size: 18, color: theme.colorScheme.onSurfaceVariant),
          const SizedBox(width: 8),
          Expanded(
            child: Text(
              text,
              style: theme.textTheme.bodySmall?.copyWith(
                color: theme.colorScheme.onSurfaceVariant,
              ),
            ),
          ),
        ],
      ),
    );
  }
}
