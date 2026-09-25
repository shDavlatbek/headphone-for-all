// BroadcastPickerFactory.swift - the `hfa/broadcast_picker` platform view (docs/CONTRACTS.md §8.3).

import Flutter
import ReplayKit
import UIKit

/// Creates [BroadcastPickerView]s for the Dart `UiKitView(viewType: 'hfa/broadcast_picker')`.
final class BroadcastPickerFactory: NSObject, FlutterPlatformViewFactory {
  /// The platform view type registered with Flutter.
  static let viewType = "hfa/broadcast_picker"

  func create(
    withFrame frame: CGRect,
    viewIdentifier viewId: Int64,
    arguments args: Any?
  ) -> FlutterPlatformView {
    BroadcastPickerView(frame: frame)
  }

  /// Dart sends its (empty) creation parameters with the standard codec.
  func createArgsCodec() -> FlutterMessageCodec & NSObjectProtocol {
    FlutterStandardMessageCodec.sharedInstance()
  }
}

/// Wraps an `RPSystemBroadcastPickerView` that offers only this app's broadcast upload
/// extension. Tapping it opens the system sheet that starts or stops the broadcast.
final class BroadcastPickerView: NSObject, FlutterPlatformView {
  private let picker: RPSystemBroadcastPickerView

  init(frame: CGRect) {
    picker = RPSystemBroadcastPickerView(frame: frame)
    picker.preferredExtension = HfaShared.broadcastExtensionBundleId
    picker.showsMicrophoneButton = false
    picker.autoresizingMask = [.flexibleWidth, .flexibleHeight]
    picker.backgroundColor = .clear
    super.init()
    // The picker's button is a plain UIButton subview; make it fill the Flutter-sized frame so
    // the whole circle drawn by Dart is tappable.
    for case let button as UIButton in picker.subviews {
      button.autoresizingMask = [.flexibleWidth, .flexibleHeight]
      button.frame = picker.bounds
    }
  }

  func view() -> UIView {
    picker
  }
}
