import 'package:flutter/material.dart';

import '../util/format.dart';

/// A horizontal level meter for a dBFS value (-60 dB .. 0 dB).
class LevelMeter extends StatelessWidget {
  /// Creates a meter showing [levelDb].
  const LevelMeter({
    super.key,
    required this.levelDb,
    this.height = 6,
    this.dimmed = false,
  });

  /// Level in dBFS (-120 = silence).
  final double levelDb;

  /// Bar height.
  final double height;

  /// Grey the bar (muted or idle source).
  final bool dimmed;

  /// Lowest level shown.
  static const floorDb = -60.0;

  /// The bar's fill fraction (0..1) for [db].
  static double fraction(double db) {
    if (!db.isFinite || db <= floorDb) return 0;
    if (db >= 0) return 1;
    return (db - floorDb) / -floorDb;
  }

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final value = fraction(levelDb);
    final color = dimmed
        ? scheme.outline
        : levelDb > -3
        ? scheme.error
        : levelDb > -12
        ? Colors.amber.shade700
        : Colors.green.shade600;
    return Semantics(
      label: 'Level',
      value: formatDb(levelDb),
      child: ClipRRect(
        borderRadius: BorderRadius.circular(height / 2),
        child: SizedBox(
          height: height,
          child: LinearProgressIndicator(
            value: value,
            color: color,
            backgroundColor: scheme.surfaceContainerHighest,
            minHeight: height,
          ),
        ),
      ),
    );
  }
}
