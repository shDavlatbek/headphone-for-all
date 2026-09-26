import Flutter
import UIKit

@main
@objc class AppDelegate: FlutterAppDelegate, FlutterImplicitEngineDelegate {
  /// `hfa/platform` channels and the broadcast picker view; kept alive with the app.
  private var platformChannel: HfaPlatformChannel?

  override func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
  ) -> Bool {
    // Before Flutter starts (and with it the Rust engine): hub discovery and advertising on iOS
    // go through native Bonjour, since mdns-sd needs the restricted multicast entitlement.
    HfaBonjourDiscovery.shared.register()
    return super.application(application, didFinishLaunchingWithOptions: launchOptions)
  }

  func didInitializeImplicitFlutterEngine(_ engineBridge: FlutterImplicitEngineBridge) {
    GeneratedPluginRegistrant.register(with: engineBridge.pluginRegistry)
    platformChannel = HfaPlatformChannel(registrar: engineBridge.applicationRegistrar)
  }
}
