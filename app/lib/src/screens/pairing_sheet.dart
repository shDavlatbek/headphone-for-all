import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';
import 'package:qr_flutter/qr_flutter.dart';

import '../state/core_providers.dart';
import '../state/pairing_controller.dart';
import '../util/format.dart';

/// Opens a pairing window and shows it in a bottom sheet (closing the sheet
/// closes the window).
Future<void> showPairingSheet(BuildContext context, WidgetRef ref) async {
  final controller = ref.read(pairingControllerProvider.notifier);
  unawaited(controller.start());
  await showModalBottomSheet<void>(
    context: context,
    isScrollControlled: true,
    showDragHandle: true,
    useSafeArea: true,
    builder: (context) => const PairingSheet(),
  );
  await controller.cancel();
}

/// "Pair a device": QR code of the pairing URI, the big 6-digit PIN and the
/// expiry countdown; reacts to `PairingCompleted` / `PairingFailed`.
class PairingSheet extends ConsumerStatefulWidget {
  /// Creates the sheet.
  const PairingSheet({super.key});

  @override
  ConsumerState<PairingSheet> createState() => _PairingSheetState();
}

class _PairingSheetState extends ConsumerState<PairingSheet> {
  Timer? _ticker;

  @override
  void initState() {
    super.initState();
    _ticker = Timer.periodic(const Duration(seconds: 1), (_) {
      if (mounted) setState(() {});
    });
  }

  @override
  void dispose() {
    _ticker?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final state = ref.watch(pairingControllerProvider);
    final now = ref.watch(clockProvider)();
    return SingleChildScrollView(
      padding: const EdgeInsets.fromLTRB(24, 0, 24, 24),
      child: Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 420),
          child: switch (state.phase) {
            PairingPhase.idle || PairingPhase.opening => const Padding(
              padding: EdgeInsets.all(48),
              child: Center(child: CircularProgressIndicator()),
            ),
            PairingPhase.failed => _Outcome(
              icon: Icons.error_outline,
              title: 'Could not open pairing',
              message: state.message ?? '',
              actionLabel: 'Try again',
              onAction: () =>
                  ref.read(pairingControllerProvider.notifier).start(),
            ),
            PairingPhase.hubStopped => _Outcome(
              icon: Icons.headset_off_outlined,
              title: 'The hub stopped',
              message: 'Start the hub again to pair a device.',
              actionLabel: 'Close',
              onAction: () => Navigator.of(context).pop(),
            ),
            PairingPhase.completed => _Outcome(
              icon: Icons.check_circle_outline,
              title: 'Paired with ${state.pairedName ?? 'a device'}',
              message:
                  'It can now send audio to this hub any time, without a PIN.',
              actionLabel: 'Done',
              onAction: () => Navigator.of(context).pop(),
            ),
            PairingPhase.waiting => _waiting(context, state, now),
          },
        ),
      ),
    );
  }

  Widget _waiting(BuildContext context, PairingState state, DateTime now) {
    final theme = Theme.of(context);
    final info = state.info;
    if (info == null) return const SizedBox.shrink();
    final left = state.secondsLeft(now);
    final expired = left == 0;
    return Column(
      mainAxisSize: MainAxisSize.min,
      children: [
        Text('Pair a device', style: theme.textTheme.headlineSmall),
        const SizedBox(height: 8),
        Text(
          'On the other device, open Send and scan this code — or pick this '
          'hub and enter the PIN.',
          textAlign: TextAlign.center,
          style: theme.textTheme.bodyMedium,
        ),
        const SizedBox(height: 16),
        Container(
          padding: const EdgeInsets.all(12),
          decoration: BoxDecoration(
            color: Colors.white,
            borderRadius: BorderRadius.circular(16),
          ),
          child: QrImageView(
            key: const Key('pairing-qr'),
            data: info.uri,
            size: 220,
            backgroundColor: Colors.white,
            semanticsLabel: 'Pairing QR code',
          ),
        ),
        const SizedBox(height: 16),
        Text('PIN', style: theme.textTheme.labelLarge),
        SelectableText(
          formatPin(info.pin),
          key: const Key('pairing-pin'),
          style: theme.textTheme.displayMedium?.copyWith(
            fontWeight: FontWeight.w600,
            letterSpacing: 6,
            fontFeatures: const [FontFeature.tabularFigures()],
            color: expired ? theme.colorScheme.outline : null,
          ),
        ),
        if (pairingUriAddress(info.uri) case final address?) ...[
          const SizedBox(height: 8),
          Text(
            'Or, on the other device, choose Add by address:',
            textAlign: TextAlign.center,
            style: theme.textTheme.bodySmall,
          ),
          SelectableText(
            address,
            key: const Key('pairing-address'),
            style: theme.textTheme.titleMedium,
          ),
        ],
        const SizedBox(height: 4),
        Text(
          expired
              ? 'This PIN has expired.'
              : 'Expires in ${formatCountdown(left)}',
          key: const Key('pairing-countdown'),
          style: theme.textTheme.bodyMedium?.copyWith(
            color: expired ? theme.colorScheme.error : null,
          ),
        ),
        if (state.message != null) ...[
          const SizedBox(height: 12),
          Card(
            color: theme.colorScheme.errorContainer,
            child: Padding(
              padding: const EdgeInsets.all(12),
              child: Text(
                'A pairing attempt failed: ${state.message}',
                style: TextStyle(color: theme.colorScheme.onErrorContainer),
              ),
            ),
          ),
        ],
        const SizedBox(height: 12),
        Wrap(
          spacing: 8,
          alignment: WrapAlignment.center,
          children: [
            TextButton.icon(
              icon: const Icon(Icons.link),
              label: const Text('Copy pairing link'),
              onPressed: () async {
                await Clipboard.setData(ClipboardData(text: info.uri));
                if (context.mounted) {
                  ScaffoldMessenger.maybeOf(context)?.showSnackBar(
                    const SnackBar(content: Text('Pairing link copied')),
                  );
                }
              },
            ),
            if (expired)
              FilledButton.icon(
                icon: const Icon(Icons.refresh),
                label: const Text('New PIN'),
                onPressed: () =>
                    ref.read(pairingControllerProvider.notifier).start(),
              ),
          ],
        ),
      ],
    );
  }
}

/// The hub's address (`host:port`) inside a pairing URI, for "Add by
/// address" where the QR code cannot be used (no camera, no multicast).
String? pairingUriAddress(String uri) {
  final parsed = Uri.tryParse(uri);
  final host = parsed?.queryParameters['h'];
  if (host == null || host.isEmpty) return null;
  final port = int.tryParse(parsed?.queryParameters['p'] ?? '') ?? 0;
  final h = host.contains(':') ? '[$host]' : host;
  return port == 0 ? h : '$h:$port';
}

class _Outcome extends StatelessWidget {
  const _Outcome({
    required this.icon,
    required this.title,
    required this.message,
    required this.actionLabel,
    required this.onAction,
  });

  final IconData icon;
  final String title;
  final String message;
  final String actionLabel;
  final VoidCallback onAction;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Padding(
      padding: const EdgeInsets.symmetric(vertical: 24),
      child: Column(
        mainAxisSize: MainAxisSize.min,
        children: [
          Icon(icon, size: 56, color: theme.colorScheme.primary),
          const SizedBox(height: 12),
          Text(
            title,
            style: theme.textTheme.titleLarge,
            textAlign: TextAlign.center,
          ),
          const SizedBox(height: 8),
          Text(message, textAlign: TextAlign.center),
          const SizedBox(height: 16),
          FilledButton(onPressed: onAction, child: Text(actionLabel)),
        ],
      ),
    );
  }
}
