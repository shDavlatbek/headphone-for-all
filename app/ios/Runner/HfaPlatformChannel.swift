// HfaPlatformChannel.swift - iOS side of `MethodChannel('hfa/platform')` and
// `EventChannel('hfa/platform/events')` (docs/CONTRACTS.md §8.3).

import AVFoundation
import Flutter
import Foundation
import os

/// Handles the `hfa/platform` method channel, forwards broadcast-extension events to Dart and
/// registers the `hfa/broadcast_picker` platform view.
///
/// | Method | iOS behaviour |
/// |---|---|
/// | `getDataDir` | `<App Group container>/hfa`; `NO_APP_GROUP` without the App Group (Simulator: Application Support `/hfa`) |
/// | `startHubService` / `stopHubService` | `AVAudioSession` `.playback` + `.mixWithOthers`, (de)activated |
/// | `writeBroadcastConfig` | writes `broadcast_config.json` into the App Group container |
/// | `captureSupport` | `{supported: true, reason: "broadcast"}` |
/// | `startSystemCapture` | `false` (capture runs in the broadcast extension) |
/// | `stopSystemCapture`, `acquireMulticastLock`, `releaseMulticastLock` | no-op |
final class HfaPlatformChannel: NSObject, FlutterStreamHandler {
  /// Name of the method channel.
  static let methodChannelName = "hfa/platform"
  /// Name of the event channel.
  static let eventChannelName = "hfa/platform/events"

  private static let log = Logger(
    subsystem: Bundle.main.bundleIdentifier ?? "headphone-for-all", category: "platform")

  private let methodChannel: FlutterMethodChannel
  private let eventChannel: FlutterEventChannel
  private var eventSink: FlutterEventSink?
  private var broadcastObserver: DarwinNotificationObserver?

  /// Registers the channels and the platform view with the application registrar of the
  /// implicit Flutter engine. Keep the returned object alive for the app's lifetime.
  init(registrar: FlutterApplicationRegistrar) {
    let messenger = registrar.messenger()
    methodChannel = FlutterMethodChannel(name: Self.methodChannelName, binaryMessenger: messenger)
    eventChannel = FlutterEventChannel(name: Self.eventChannelName, binaryMessenger: messenger)
    super.init()
    methodChannel.setMethodCallHandler { [weak self] call, result in
      guard let self else {
        result(FlutterMethodNotImplemented)
        return
      }
      self.handle(call, result: result)
    }
    eventChannel.setStreamHandler(self)
    registrar.register(BroadcastPickerFactory(), withId: BroadcastPickerFactory.viewType)
    broadcastObserver = DarwinNotificationObserver(
      names: [HfaShared.broadcastStartedNotification, HfaShared.broadcastFinishedNotification]
    ) { [weak self] name in
      self?.broadcastNotification(name)
    }
  }

  // MARK: - Method channel

  private func handle(_ call: FlutterMethodCall, result: @escaping FlutterResult) {
    switch call.method {
    case "getDataDir":
      result(dataDirResult())
    case "startHubService":
      result(setAudioSessionActive(true))
    case "stopHubService":
      result(setAudioSessionActive(false))
    case "writeBroadcastConfig":
      result(writeBroadcastConfig(call.arguments))
    case "captureSupport":
      result(["supported": true, "reason": "broadcast"])
    case "startSystemCapture":
      // iOS has no in-app system capture: the user starts the broadcast extension with the
      // `hfa/broadcast_picker` view instead.
      result(false)
    case "stopSystemCapture", "acquireMulticastLock", "releaseMulticastLock":
      result(nil)
    default:
      result(FlutterMethodNotImplemented)
    }
  }

  /// `<App Group container>/hfa` (see `resolveDataDir(shared:privateFallback:)`).
  private func dataDirResult() -> Any {
    let fallback: (() throws -> URL)? = Self.allowsPrivateDataDir ? Self.privateDataDir : nil
    return Self.resolveDataDir(shared: HfaShared.sharedDataDir, privateFallback: fallback)
  }

  /// Message of the `NO_APP_GROUP` errors (shown by the app's start-up error screen).
  static var noAppGroupMessage: String {
    "The App Group \(HfaShared.appGroupId) is not available, so the broadcast extension could not "
      + "use this app's pairings. Sign the Runner and HfaBroadcast targets with a team whose App IDs "
      + "have this App Group (set HFA_BUNDLE_ID in ios/Identity.xcconfig for your own team)."
  }

  /// Whether a build without the App Group may keep its data in the app's own container. Only
  /// the Simulator, where builds are usually unsigned (and ReplayKit broadcasts do not run).
  static var allowsPrivateDataDir: Bool {
    #if targetEnvironment(simulator)
      return true
    #else
      return false
    #endif
  }

  /// The data directory answer of `getDataDir`: the path from `shared` (the App Group's `hfa`
  /// directory), else the path from `privateFallback`, else `FlutterError("NO_APP_GROUP")`.
  ///
  /// Without the App Group a device build must not start on a private directory: identity and
  /// pairings stored there are invisible to the broadcast extension, so every broadcast would
  /// fail later. The error makes the Dart bootstrap show its start-up error instead
  /// (docs/CONTRACTS.md §8.7: on iOS a `PlatformException` from `getDataDir` is fatal).
  /// I/O failures → `FlutterError("DATA_DIR")`.
  static func resolveDataDir(shared: () throws -> URL?, privateFallback: (() throws -> URL)?)
    -> Any
  {
    do {
      if let dir = try shared() {
        return dir.path
      }
      guard let privateFallback else {
        log.error(
          "App Group \(HfaShared.appGroupId, privacy: .public) unavailable: check the signing and entitlements"
        )
        return FlutterError(code: "NO_APP_GROUP", message: noAppGroupMessage, details: nil)
      }
      log.warning(
        "App Group \(HfaShared.appGroupId, privacy: .public) unavailable; using Application Support (Simulator)"
      )
      return try privateFallback().path
    } catch {
      return FlutterError(
        code: "DATA_DIR", message: "cannot create the data directory: \(error.localizedDescription)",
        details: nil)
    }
  }

  /// Application Support `/hfa`, created if needed (Simulator builds without the App Group).
  private static func privateDataDir() throws -> URL {
    let support = try FileManager.default.url(
      for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
    let dir = support.appendingPathComponent(HfaShared.dataDirName, isDirectory: true)
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    return dir
  }

  /// Hub mode: a `.playback` session that mixes with other apps keeps the hub playing in the
  /// background (`UIBackgroundModes: audio`) without silencing the phone's own audio.
  private func setAudioSessionActive(_ active: Bool) -> Any? {
    let session = AVAudioSession.sharedInstance()
    do {
      if active {
        try session.setCategory(.playback, mode: .default, options: [.mixWithOthers])
        try session.setActive(true)
      } else {
        try session.setActive(false, options: [.notifyOthersOnDeactivation])
      }
      return nil
    } catch {
      return FlutterError(
        code: "AUDIO_SESSION",
        message: "cannot \(active ? "activate" : "deactivate") the audio session: \(error.localizedDescription)",
        details: nil)
    }
  }

  /// Writes `broadcast_config.json` (`{hubHost, hubPort, hubDeviceId, hubKey, label}` from Dart,
  /// plus `data_dir`) into the App Group container for the extension.
  private func writeBroadcastConfig(_ arguments: Any?) -> Any? {
    guard let args = arguments as? [String: Any] else {
      return FlutterError(code: "BAD_ARGS", message: "expected a map", details: nil)
    }
    let hubHost = (args["hubHost"] as? String)?.trimmingCharacters(in: .whitespaces) ?? ""
    let hubPort = (args["hubPort"] as? NSNumber)?.intValue ?? 0
    let hubDeviceId = nonEmpty(args["hubDeviceId"])
    let hubKey = nonEmpty(args["hubKey"])
    let label = (args["label"] as? String) ?? ""
    guard (0...65_535).contains(hubPort) else {
      return FlutterError(code: "BAD_ARGS", message: "hubPort out of range: \(hubPort)", details: nil)
    }
    // The extension cannot look for the hub: mDNS needs the restricted multicast entitlement
    // on iOS, so a configuration without an address could only fail after the broadcast started.
    guard !hubHost.isEmpty else {
      return FlutterError(
        code: "BAD_ARGS",
        message:
          "The hub's address is unknown, and this device cannot look for hubs on the network. Add the hub by address or scan its QR code.",
        details: nil)
    }
    let dataDir: URL
    let configURL: URL
    do {
      guard let dir = try HfaShared.sharedDataDir(),
        let url = HfaShared.containerFile(HfaShared.broadcastConfigFileName)
      else {
        return FlutterError(
          code: "NO_APP_GROUP",
          message: Self.noAppGroupMessage,
          details: nil)
      }
      dataDir = dir
      configURL = url
    } catch {
      return FlutterError(
        code: "DATA_DIR", message: "cannot create the data directory: \(error.localizedDescription)",
        details: nil)
    }
    let config = BroadcastConfig(
      hubHost: hubHost, hubPort: hubPort, hubDeviceId: hubDeviceId, hubKey: hubKey, label: label,
      dataDir: dataDir.path)
    do {
      try config.jsonData().write(to: configURL, options: .atomic)
      return nil
    } catch {
      return FlutterError(
        code: "WRITE_FAILED",
        message: "cannot write \(HfaShared.broadcastConfigFileName): \(error.localizedDescription)",
        details: nil)
    }
  }

  private func nonEmpty(_ value: Any?) -> String? {
    guard let s = (value as? String)?.trimmingCharacters(in: .whitespaces), !s.isEmpty else {
      return nil
    }
    return s
  }

  // MARK: - Event channel

  func onListen(withArguments arguments: Any?, eventSink events: @escaping FlutterEventSink)
    -> FlutterError?
  {
    eventSink = events
    return nil
  }

  func onCancel(withArguments arguments: Any?) -> FlutterError? {
    eventSink = nil
    return nil
  }

  /// Turns an extension notification into a `{type, message?}` event.
  private func broadcastNotification(_ name: String) {
    var event: [String: Any]
    switch name {
    case HfaShared.broadcastStartedNotification:
      event = ["type": "broadcastStarted"]
    case HfaShared.broadcastFinishedNotification:
      event = ["type": "broadcastFinished"]
      if let status = BroadcastStatus.read(), status.state == "finished",
        let message = status.message, !message.isEmpty
      {
        event["message"] = message
      }
    default:
      return
    }
    eventSink?(event)
  }
}

/// Observes Darwin (cross-process) notifications and calls `handler` on the main thread.
final class DarwinNotificationObserver {
  private let names: [String]
  private let handler: (String) -> Void

  init(names: [String], handler: @escaping (String) -> Void) {
    self.names = names
    self.handler = handler
    let center = CFNotificationCenterGetDarwinNotifyCenter()
    let observer = Unmanaged.passUnretained(self).toOpaque()
    for name in names {
      CFNotificationCenterAddObserver(
        center,
        observer,
        { _, observer, name, _, _ in
          // A C callback cannot capture context: the observer pointer carries `self`, which
          // stays valid until `deinit` removes the registration.
          guard let observer, let name else { return }
          let me = Unmanaged<DarwinNotificationObserver>.fromOpaque(observer)
            .takeUnretainedValue()
          me.deliver(name.rawValue as String)
        },
        name as CFString,
        nil,
        .deliverImmediately
      )
    }
  }

  deinit {
    CFNotificationCenterRemoveEveryObserver(
      CFNotificationCenterGetDarwinNotifyCenter(), Unmanaged.passUnretained(self).toOpaque())
  }

  private func deliver(_ name: String) {
    if Thread.isMainThread {
      handler(name)
    } else {
      DispatchQueue.main.async { [handler] in handler(name) }
    }
  }
}
