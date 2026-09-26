import 'package:flutter/material.dart';

import '../api/hfa_api.dart';
import '../util/format.dart';
import 'level_meter.dart';

/// Maximum gain offered by the sliders (the core accepts up to 4.0).
const maxSliderGain = 2.0;

/// One incoming stream on the hub: meter, volume, mute, priority and stats.
class SourceTile extends StatefulWidget {
  /// Creates a tile for [source].
  const SourceTile({
    super.key,
    required this.source,
    required this.onGainChanged,
    required this.onMutedChanged,
    required this.onPriorityChanged,
  });

  /// The stream.
  final SourceDto source;

  /// Called when the user releases the volume slider.
  final ValueChanged<double> onGainChanged;

  /// Called when mute is toggled.
  final ValueChanged<bool> onMutedChanged;

  /// Called when priority is toggled.
  final ValueChanged<bool> onPriorityChanged;

  @override
  State<SourceTile> createState() => _SourceTileState();
}

class _SourceTileState extends State<SourceTile> {
  /// The slider value while dragging (the core's value otherwise), so the
  /// periodic refresh does not make the thumb jump.
  double? _dragGain;

  @override
  Widget build(BuildContext context) {
    final s = widget.source;
    final theme = Theme.of(context);
    final scheme = theme.colorScheme;
    final dimmed = s.muted || !s.active;
    final gain = (_dragGain ?? s.gain).clamp(0.0, maxSliderGain);
    final id = s.streamId;
    return Card(
      key: Key('source-$id'),
      margin: const EdgeInsets.symmetric(vertical: 6),
      child: Padding(
        padding: const EdgeInsets.fromLTRB(16, 12, 8, 12),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Row(
              children: [
                Icon(
                  platformIcon(s.platform),
                  color: dimmed ? scheme.outline : scheme.primary,
                ),
                const SizedBox(width: 12),
                Expanded(
                  child: Column(
                    crossAxisAlignment: CrossAxisAlignment.start,
                    children: [
                      Text(
                        s.deviceName,
                        style: theme.textTheme.titleMedium,
                        overflow: TextOverflow.ellipsis,
                      ),
                      Text(
                        s.active ? s.label : '${s.label} · idle',
                        style: theme.textTheme.bodySmall?.copyWith(
                          color: scheme.onSurfaceVariant,
                        ),
                        overflow: TextOverflow.ellipsis,
                      ),
                    ],
                  ),
                ),
                IconButton(
                  key: Key('priority-$id'),
                  tooltip: s.priority
                      ? 'Priority (ducks the others) — tap to clear'
                      : 'Make priority (ducks the others)',
                  isSelected: s.priority,
                  icon: const Icon(Icons.star_border),
                  selectedIcon: Icon(Icons.star, color: Colors.amber.shade700),
                  onPressed: () => widget.onPriorityChanged(!s.priority),
                ),
                IconButton(
                  key: Key('mute-$id'),
                  tooltip: s.muted ? 'Unmute' : 'Mute',
                  isSelected: s.muted,
                  icon: const Icon(Icons.volume_up_outlined),
                  selectedIcon: Icon(Icons.volume_off, color: scheme.error),
                  onPressed: () => widget.onMutedChanged(!s.muted),
                ),
              ],
            ),
            const SizedBox(height: 10),
            Padding(
              padding: const EdgeInsets.only(right: 8),
              child: LevelMeter(levelDb: s.levelDb, dimmed: dimmed),
            ),
            Row(
              children: [
                Expanded(
                  child: Slider(
                    key: Key('gain-$id'),
                    value: gain,
                    max: maxSliderGain,
                    divisions: 40,
                    label: formatGain(gain),
                    semanticFormatterCallback: formatGain,
                    onChanged: (v) => setState(() => _dragGain = v),
                    onChangeEnd: (v) {
                      setState(() => _dragGain = null);
                      widget.onGainChanged(v);
                    },
                  ),
                ),
                SizedBox(
                  width: 48,
                  child: Text(
                    formatGain(gain),
                    textAlign: TextAlign.end,
                    style: theme.textTheme.bodySmall,
                  ),
                ),
                const SizedBox(width: 8),
              ],
            ),
            Wrap(
              spacing: 12,
              runSpacing: 4,
              children: [
                _Stat(label: 'loss', value: formatPercent(s.lossPct)),
                _Stat(label: 'jitter', value: formatMs(s.jitterMs)),
                _Stat(label: 'buffer', value: formatMs(s.bufferMs)),
                _Stat(label: 'latency', value: formatMs(s.latencyMs)),
                _Stat(label: 'level', value: formatDb(s.levelDb)),
              ],
            ),
          ],
        ),
      ),
    );
  }
}

class _Stat extends StatelessWidget {
  const _Stat({required this.label, required this.value});

  final String label;
  final String value;

  @override
  Widget build(BuildContext context) {
    final theme = Theme.of(context);
    return Text.rich(
      TextSpan(
        children: [
          TextSpan(
            text: '$label ',
            style: TextStyle(color: theme.colorScheme.onSurfaceVariant),
          ),
          TextSpan(text: value),
        ],
      ),
      style: theme.textTheme.bodySmall,
    );
  }
}
