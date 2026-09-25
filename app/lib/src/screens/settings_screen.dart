import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_riverpod/flutter_riverpod.dart';

import '../api/hfa_api.dart';
import '../state/core_providers.dart';
import '../state/hub_controller.dart';
import '../state/sender_controller.dart';
import '../state/settings_controller.dart';
import '../util/format.dart';
import '../widgets/dialogs.dart';

/// Bitrates offered in the picker (bit/s).
const bitrateChoices = [64000, 96000, 128000, 160000, 192000, 256000, 320000];

/// Bounds of the jitter-buffer range slider (ms).
const jitterSliderMin = 5.0;

/// Upper bound of the jitter-buffer range slider (ms).
const jitterSliderMax = 500.0;

/// What saving tells the user: running engines keep their settings until
/// restarted (§8.5), the hub for port, jitter and output, the sender for
/// bitrate, frame length and FEC.
String savedSettingsMessage({
  required bool hubRunning,
  required bool senderLive,
}) => switch ((hubRunning, senderLive)) {
  (true, true) =>
    'Saved. Restart the hub, and stop and start sending, to apply the '
        'changes.',
  (true, false) => 'Saved. Restart the hub to apply the changes.',
  (false, true) => 'Saved. Stop and start sending to apply the changes.',
  (false, false) => 'Settings saved.',
};

/// Device name, audio quality, network, output device and trusted devices.
class SettingsScreen extends ConsumerWidget {
  /// Creates the screen.
  const SettingsScreen({super.key});

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final settings = ref.watch(settingsControllerProvider);
    return settings.when(
      loading: () => const Center(child: CircularProgressIndicator()),
      error: (e, _) => Center(
        child: Padding(
          padding: const EdgeInsets.all(24),
          child: Text('Could not load the settings: ${describeError(e)}'),
        ),
      ),
      data: (s) => ListView(
        padding: const EdgeInsets.all(16),
        children: [
          // Re-created when the saved settings change (e.g. after a save).
          SettingsForm(key: ValueKey(s), initial: s),
          const SizedBox(height: 16),
          const TrustedDevicesCard(),
        ],
      ),
    );
  }
}

/// The editable settings; "Save" validates them in the core.
class SettingsForm extends ConsumerStatefulWidget {
  /// Creates the form with the saved [initial] settings.
  const SettingsForm({super.key, required this.initial});

  /// The saved settings.
  final SettingsDto initial;

  @override
  ConsumerState<SettingsForm> createState() => _SettingsFormState();
}

class _SettingsFormState extends ConsumerState<SettingsForm> {
  late final TextEditingController _name;
  late final TextEditingController _port;
  late int _bitrate;
  late int _frameMs;
  late bool _fec;
  late RangeValues _jitter;
  late double _jitterMin;
  late double _jitterMax;
  String? _output;
  bool _saving = false;
  String? _nameError;
  String? _portError;

  @override
  void initState() {
    super.initState();
    final s = widget.initial;
    _name = TextEditingController(text: s.deviceName);
    _port = TextEditingController(text: s.port == 0 ? '' : '${s.port}');
    _bitrate = s.bitrate;
    _frameMs = s.frameMs;
    _fec = s.fec;
    // Widen the slider for values saved elsewhere (e.g. by the CLI) so that
    // opening the screen never changes them.
    _jitterMin = s.jitterMinMs < jitterSliderMin
        ? s.jitterMinMs.toDouble()
        : jitterSliderMin;
    _jitterMax = s.jitterMaxMs > jitterSliderMax
        ? s.jitterMaxMs.toDouble()
        : jitterSliderMax;
    _jitter = RangeValues(s.jitterMinMs.toDouble(), s.jitterMaxMs.toDouble());
    _output = s.outputDevice;
  }

  @override
  void dispose() {
    _name.dispose();
    _port.dispose();
    super.dispose();
  }

  SettingsDto _current() => SettingsDto(
    deviceName: _name.text.trim(),
    port: int.tryParse(_port.text.trim()) ?? 0,
    bitrate: _bitrate,
    frameMs: _frameMs,
    fec: _fec,
    jitterMinMs: _jitter.start.round(),
    jitterMaxMs: _jitter.end.round(),
    outputDevice: _output,
  );

  Future<void> _save() async {
    final name = _name.text.trim();
    final nameError = name.isEmpty || name.length > 64
        ? 'Use 1 to 64 characters'
        : null;
    // SettingsDto.port is a u16: an out-of-range value would be truncated
    // on its way to the core and saved as another port.
    final portError = validatePort(_port.text);
    if (nameError != null || portError != null) {
      setState(() {
        _nameError = nameError;
        _portError = portError;
      });
      return;
    }
    setState(() {
      _nameError = null;
      _portError = null;
      _saving = true;
    });
    // The notifiers outlive this form: a save that changes the settings
    // replaces it (it is keyed by the saved value), so the snack bar's
    // action must not use this widget's `ref`.
    final hub = ref.read(hubControllerProvider.notifier);
    final sender = ref.read(senderControllerProvider.notifier);
    try {
      await ref.read(settingsControllerProvider.notifier).save(_current());
      if (!mounted) return;
      final hubRunning = ref.read(hubControllerProvider).running;
      final senderLive = ref.read(senderControllerProvider).isLive;
      showMessage(
        context,
        savedSettingsMessage(hubRunning: hubRunning, senderLive: senderLive),
        action: hubRunning
            ? SnackBarAction(
                label: 'Restart hub',
                onPressed: () => unawaited(hub.restart()),
              )
            : senderLive
            ? SnackBarAction(
                label: 'Restart sending',
                onPressed: () => unawaited(sender.restart()),
              )
            : null,
      );
    } catch (e) {
      if (mounted) showMessage(context, describeError(e));
    } finally {
      if (mounted) setState(() => _saving = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    final info = ref.watch(appInfoProvider);
    final bitrates = {...bitrateChoices, _bitrate}.toList()..sort();
    return Card(
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text('This device', style: theme.textTheme.titleMedium),
            const SizedBox(height: 8),
            TextField(
              key: const Key('device-name'),
              controller: _name,
              maxLength: 64,
              decoration: InputDecoration(
                labelText: 'Device name',
                helperText: 'Shown on hubs and in discovery',
                errorText: _nameError,
              ),
            ),
            const SizedBox(height: 16),
            Text('Audio quality', style: theme.textTheme.titleMedium),
            const SizedBox(height: 8),
            Row(
              children: [
                const Expanded(child: Text('Bitrate')),
                DropdownButton<int>(
                  key: const Key('bitrate'),
                  value: _bitrate,
                  items: [
                    for (final b in bitrates)
                      DropdownMenuItem(value: b, child: Text(formatBitrate(b))),
                  ],
                  onChanged: (b) => setState(() => _bitrate = b ?? _bitrate),
                ),
              ],
            ),
            const SizedBox(height: 8),
            Row(
              children: [
                const Expanded(child: Text('Frame length')),
                SegmentedButton<int>(
                  key: const Key('frame-ms'),
                  segments: const [
                    ButtonSegment(value: 10, label: Text('10 ms')),
                    ButtonSegment(value: 20, label: Text('20 ms')),
                  ],
                  selected: {_frameMs},
                  onSelectionChanged: (v) => setState(() => _frameMs = v.first),
                ),
              ],
            ),
            Text(
              '10 ms = lower latency; 20 ms = fewer packets on busy Wi-Fi.',
              style: theme.textTheme.bodySmall,
            ),
            SwitchListTile(
              key: const Key('fec'),
              contentPadding: EdgeInsets.zero,
              title: const Text('Forward error correction'),
              subtitle: const Text('Recovers lost packets at a small cost'),
              value: _fec,
              onChanged: (v) => setState(() => _fec = v),
            ),
            const SizedBox(height: 8),
            Text(
              'Jitter buffer: ${_jitter.start.round()}–${_jitter.end.round()} ms',
              key: const Key('jitter-label'),
            ),
            RangeSlider(
              key: const Key('jitter'),
              values: _jitter,
              min: _jitterMin,
              max: _jitterMax,
              labels: RangeLabels(
                '${_jitter.start.round()} ms',
                '${_jitter.end.round()} ms',
              ),
              onChanged: (v) => setState(() => _jitter = v),
            ),
            Text(
              'The hub adapts between these bounds: a wider range survives '
              'bad Wi-Fi, a lower minimum means less delay.',
              style: theme.textTheme.bodySmall,
            ),
            const SizedBox(height: 16),
            Text('Network', style: theme.textTheme.titleMedium),
            TextField(
              key: const Key('port'),
              controller: _port,
              keyboardType: TextInputType.number,
              maxLength: 5,
              inputFormatters: [FilteringTextInputFormatter.digitsOnly],
              decoration: InputDecoration(
                labelText: 'Hub port',
                helperText: 'Empty = any free port',
                errorText: _portError,
                counterText: '',
              ),
            ),
            if (info.isDesktop) ...[
              const SizedBox(height: 16),
              Text('Hub output', style: theme.textTheme.titleMedium),
              _OutputPicker(
                value: _output,
                onChanged: (v) => setState(() => _output = v),
              ),
            ],
            const SizedBox(height: 16),
            Align(
              alignment: Alignment.centerRight,
              child: FilledButton.icon(
                key: const Key('save-settings'),
                icon: _saving
                    ? const SizedBox.square(
                        dimension: 18,
                        child: CircularProgressIndicator(strokeWidth: 2),
                      )
                    : const Icon(Icons.save_outlined),
                label: const Text('Save'),
                onPressed: _saving ? null : _save,
              ),
            ),
          ],
        ),
      ),
    );
  }
}

class _OutputPicker extends ConsumerWidget {
  const _OutputPicker({required this.value, required this.onChanged});

  final String? value;
  final ValueChanged<String?> onChanged;

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final devices = ref.watch(outputDevicesProvider);
    return devices.when(
      loading: () => const LinearProgressIndicator(),
      error: (e, _) => Text('Could not list outputs: ${describeError(e)}'),
      data: (list) {
        final names = {...list, ?value}.toList();
        return DropdownButton<String?>(
          key: const Key('output-device'),
          isExpanded: true,
          value: value,
          items: [
            const DropdownMenuItem<String?>(
              value: null,
              child: Text('System default output'),
            ),
            for (final name in names)
              DropdownMenuItem<String?>(value: name, child: Text(name)),
          ],
          onChanged: onChanged,
        );
      },
    );
  }
}

/// The trust store with "Forget".
class TrustedDevicesCard extends ConsumerWidget {
  /// Creates the card.
  const TrustedDevicesCard({super.key});

  Future<void> _forget(
    BuildContext context,
    WidgetRef ref,
    TrustedPeerDto peer,
  ) async {
    final ok = await showConfirmDialog(
      context,
      title: 'Forget ${peer.name}?',
      message: ref.read(hubControllerProvider).running
          ? 'It will need a new PIN to connect again. The hub restarts to '
                'apply this, so every device reconnects.'
          : 'It will need a new PIN to connect again.',
      confirmLabel: 'Forget',
    );
    if (!ok) return;
    try {
      final restarted = await ref
          .read(trustedPeersProvider.notifier)
          .forget(peer.deviceId);
      if (!context.mounted) return;
      final hubError = ref.read(hubControllerProvider).error;
      showMessage(
        context,
        !restarted
            ? 'Forgot ${peer.name}.'
            : hubError == null
            ? 'Forgot ${peer.name}. The hub restarted.'
            : 'Forgot ${peer.name}, but the hub did not restart: $hubError',
      );
    } catch (e) {
      if (context.mounted) showMessage(context, describeError(e));
    }
  }

  @override
  Widget build(BuildContext context, WidgetRef ref) {
    final theme = Theme.of(context);
    final peers = ref.watch(trustedPeersProvider);
    return Card(
      child: Padding(
        padding: const EdgeInsets.symmetric(vertical: 8),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Padding(
              padding: const EdgeInsets.fromLTRB(16, 8, 16, 8),
              child: Text(
                'Trusted devices',
                style: theme.textTheme.titleMedium,
              ),
            ),
            ...peers.when(
              loading: () => const [LinearProgressIndicator()],
              error: (e, _) => [
                ListTile(title: Text('Could not load: ${describeError(e)}')),
              ],
              data: (list) => list.isEmpty
                  ? const [
                      ListTile(
                        title: Text('No paired devices yet'),
                        subtitle: Text(
                          'Devices pair once with a PIN or QR code.',
                        ),
                      ),
                    ]
                  : [
                      for (final peer in list)
                        ListTile(
                          key: Key('peer-${peer.deviceId}'),
                          leading: const Icon(Icons.verified_user_outlined),
                          title: Text(peer.name),
                          subtitle: Text(
                            '${peer.deviceId} · paired '
                            '${formatUnixDate(peer.pairedAtUnix)}',
                          ),
                          trailing: TextButton(
                            key: Key('forget-${peer.deviceId}'),
                            onPressed: () => _forget(context, ref, peer),
                            child: const Text('Forget'),
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
