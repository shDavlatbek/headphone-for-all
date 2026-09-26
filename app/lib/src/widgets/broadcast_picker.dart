import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

/// The iOS system broadcast picker (`RPSystemBroadcastPickerView`), shown
/// through the `hfa/broadcast_picker` platform view registered by the iOS
/// runner (CONTRACTS.md §8.3). Tapping it opens the system sheet that starts
/// the app's broadcast upload extension.
///
/// On any other platform (and in tests) it renders an explanatory
/// placeholder, since the view does not exist there.
class BroadcastPicker extends StatelessWidget {
  /// Creates the picker.
  const BroadcastPicker({super.key, this.size = 64});

  /// Name of the platform view.
  static const viewType = 'hfa/broadcast_picker';

  /// Side of the (square) picker button.
  final double size;

  @override
  Widget build(BuildContext context) {
    final scheme = Theme.of(context).colorScheme;
    final Widget picker;
    if (!kIsWeb && defaultTargetPlatform == TargetPlatform.iOS) {
      picker = const UiKitView(
        viewType: viewType,
        creationParamsCodec: StandardMessageCodec(),
      );
    } else {
      picker = Icon(Icons.screen_share_outlined, color: scheme.outline);
    }
    return Semantics(
      button: true,
      label: 'Start or stop the screen broadcast',
      child: Container(
        width: size,
        height: size,
        decoration: BoxDecoration(
          color: scheme.primaryContainer,
          shape: BoxShape.circle,
        ),
        child: ClipOval(child: picker),
      ),
    );
  }
}
