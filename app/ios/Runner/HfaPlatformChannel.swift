// HfaPlatformChannel.swift - iOS side of `MethodChannel('hfa/platform')` and
// `EventChannel('hfa/platform/events')` (docs/CONTRACTS.md §8.3).

import AVFoundation
import Flutter
import Foundation
import UIKit
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
/// | `getBroadcastStatus` | `{state, message?, hubName?, updatedAtMs, broadcasting, timestamp}`, `nil` without a status file (see `BroadcastStatusReport.answer`) |
///
/// Broadcast events: `broadcastStarted` / `broadcastFinished` follow the extension's Darwin
/// notifications and are re-synchronized with `broadcast_status.json` and the screen capture
/// state when a listener starts, the app becomes active or the capture state changes (see
/// `syncBroadcastState()`). `broadcastStatus` events (`{type, state, message?, hubName?,
/// updatedAtMs, broadcasting}`, see `BroadcastStatusReport.event`) follow every change of the
/// status file; a running state is only sent while the screen is captured.
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
  /// `NotificationCenter` observers that trigger `syncBroadcastState()`.
  private var syncObservers: [NSObjectProtocol] = []
  /// What Dart was last told: `true` after `broadcastStarted`, `false` after `broadcastFinished`
  /// and when a listener starts (Dart then assumes that no broadcast runs).
  private var reportedBroadcasting = false
  /// Pending re-check of a broadcast that looks `vanished`.
  private var vanishCheck: DispatchWorkItem?
  /// The last `broadcastStatus` event sent (to send only changes); reset for a new listener.
  private var reportedStatus: [String: AnyHashable]?

  /// How long a broadcast may look `vanished` before it counts as ended: the extension writes
  /// its `stopped` status after ReplayKit stopped capturing the screen and after it stopped the
  /// Rust sender (`hfa_ext_sender_stop` waits up to 2 s for its runtime).
  static let vanishGrace: TimeInterval = 8

  /// Why a broadcast ended when ReplayKit stopped the extension without `broadcastFinished`.
  static let vanishedMessage =
    "The broadcast stopped unexpectedly: iOS ended the broadcast extension (for example because it used too much memory). Start it again."

  /// Message of the `BAD_ARGS` error of `writeBroadcastConfig` without a hub address.
  static let hubAddressUnknownMessage =
    "The hub's address is not known yet. Pick the hub from the list of hubs found on this network, scan its QR code or add it by address."

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
      names: [
        HfaShared.broadcastStartedNotification, HfaShared.broadcastFinishedNotification,
        HfaShared.broadcastStatusNotification,
      ]
    ) { [weak self] name in
      self?.broadcastNotification(name)
    }
    let center = NotificationCenter.default
    for name in [UIApplication.didBecomeActiveNotification, UIScreen.capturedDidChangeNotification] {
      syncObservers.append(
        center.addObserver(forName: name, object: nil, queue: .main) { [weak self] _ in
          self?.syncBroadcastState()
        })
    }
  }

  deinit {
    for observer in syncObservers {
      NotificationCenter.default.removeObserver(observer)
    }
    vanishCheck?.cancel()
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
    case "getBroadcastStatus":
      result(broadcastStatusResult())
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
    // The extension cannot look for the hub itself (only the app browses, through
    // HfaBonjourDiscovery), so a configuration without an address could only fail after the
    // broadcast started.
    guard !hubHost.isEmpty else {
      return FlutterError(
        code: "BAD_ARGS",
        message: Self.hubAddressUnknownMessage,
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
    // A new listener (the app just started, or Dart subscribed again) assumes that no broadcast
    // runs; the extension outlives the app, so tell it about one that does.
    reportedBroadcasting = false
    reportedStatus = nil
    DispatchQueue.main.async { [weak self] in self?.syncBroadcastState() }
    return nil
  }

  func onCancel(withArguments arguments: Any?) -> FlutterError? {
    eventSink = nil
    return nil
  }

  /// Turns an extension notification into a `{type, message?}` event, followed by a
  /// `broadcastStatus` event with the new status.
  private func broadcastNotification(_ name: String) {
    defer { reportBroadcastStatus() }
    var event: [String: Any]
    switch name {
    case HfaShared.broadcastStartedNotification:
      cancelVanishCheck()
      reportedBroadcasting = true
      event = ["type": "broadcastStarted"]
    case HfaShared.broadcastFinishedNotification:
      cancelVanishCheck()
      reportedBroadcasting = false
      event = ["type": "broadcastFinished"]
      if let status = BroadcastStatus.read(), !status.isActive,
        let message = status.message, !message.isEmpty
      {
        event["message"] = message
      }
    default:
      return
    }
    eventSink?(event)
  }

  // MARK: - Broadcast state

  /// Brings Dart's broadcast state in line with the extension's status file and the screen
  /// capture state. The Darwin notifications cover the normal cases; this covers a broadcast
  /// that runs while the app starts (the notification was posted before), and an extension that
  /// ReplayKit killed (memory limit, crash) without calling `broadcastFinished`, which never
  /// writes a final status nor posts its notification.
  private func syncBroadcastState() {
    // A notification may have been missed while the app was suspended.
    reportBroadcastStatus()
    guard let captured = Self.screenCaptured() else { return }
    switch BroadcastSync.evaluate(status: BroadcastStatus.read(), screenCaptured: captured) {
    case .running:
      cancelVanishCheck()
      report(broadcasting: true, message: nil)
    case let .idle(message):
      cancelVanishCheck()
      report(broadcasting: false, message: message)
    case .vanished:
      scheduleVanishCheck()
    }
  }

  /// Sends `broadcastStarted` / `broadcastFinished` when it changes what Dart was told.
  private func report(broadcasting: Bool, message: String?) {
    guard broadcasting != reportedBroadcasting else { return }
    reportedBroadcasting = broadcasting
    var event: [String: Any] = ["type": broadcasting ? "broadcastStarted" : "broadcastFinished"]
    if let message, !message.isEmpty {
      event["message"] = message
    }
    eventSink?(event)
  }

  /// A `vanished` broadcast that still looks so after `vanishGrace` ended without the extension
  /// noticing: record a `failed` status (so a later app start does not take the stale running
  /// state for a running broadcast) and tell Dart.
  private func scheduleVanishCheck() {
    guard vanishCheck == nil else { return }
    let check = DispatchWorkItem { [weak self] in
      guard let self else { return }
      self.vanishCheck = nil
      let status = BroadcastStatus.read()
      guard let captured = Self.screenCaptured(),
        BroadcastSync.evaluate(status: status, screenCaptured: captured) == .vanished
      else {
        self.syncBroadcastState()
        return
      }
      Self.log.error("the broadcast extension ended without finishing the broadcast")
      BroadcastStatus(
        state: BroadcastStatus.failed, message: Self.vanishedMessage,
        timestamp: Date().timeIntervalSince1970, hubName: status?.hubName
      ).write()
      self.report(broadcasting: false, message: Self.vanishedMessage)
      self.reportBroadcastStatus()
    }
    vanishCheck = check
    DispatchQueue.main.asyncAfter(deadline: .now() + Self.vanishGrace, execute: check)
  }

  private func cancelVanishCheck() {
    vanishCheck?.cancel()
    vanishCheck = nil
  }

  /// Sends a `broadcastStatus` event when what Dart should see (`BroadcastStatusReport.event`)
  /// differs from the last one sent. A running state of an extension that looks `vanished` is
  /// held back: the extension's final status or the vanish check's `failed` follows.
  private func reportBroadcastStatus() {
    guard let eventSink, let status = BroadcastStatus.read() else { return }
    let sync = BroadcastSync.evaluate(status: status, screenCaptured: Self.screenCaptured() ?? false)
    guard let fields = BroadcastStatusReport.event(of: status, sync: sync),
      fields != reportedStatus
    else { return }
    reportedStatus = fields
    var event = BroadcastStatusReport.channelMap(fields)
    event["type"] = "broadcastStatus"
    eventSink(event)
  }

  /// `getBroadcastStatus`: `nil` without a status file, else `BroadcastStatusReport.answer`
  /// (the contract fields, `broadcasting` and `timestamp`).
  private func broadcastStatusResult() -> Any? {
    guard let status = BroadcastStatus.read() else { return nil }
    let sync = BroadcastSync.evaluate(status: status, screenCaptured: Self.screenCaptured() ?? false)
    return BroadcastStatusReport.channelMap(BroadcastStatusReport.answer(of: status, sync: sync))
  }

  /// Whether the screen is being captured (a broadcast, but also a recording or mirroring), or
  /// `nil` when the app has no window scene to ask.
  private static func screenCaptured() -> Bool? {
    let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
    guard !scenes.isEmpty else { return nil }
    return scenes.contains { scene in
      if #available(iOS 17.0, *), scene.traitCollection.sceneCaptureState == .active {
        return true
      }
      return scene.screen.isCaptured
    }
  }
}

/// The broadcast status as Dart sees it (`getBroadcastStatus`, `broadcastStatus` events;
/// docs/CONTRACTS.md §8.3): `{state, message?, hubName?, updatedAtMs}`.
enum BroadcastStatusReport {
  /// Every `state` Dart can receive.
  static let states: Set<String> = [
    "idle", "connecting", "streaming", "reconnecting", "failed", "stopped",
  ]

  /// The Dart state of a status file: the extension's state, with the legacy `started` →
  /// `connecting` and `finished` → `failed` (with a message) or `stopped`; unknown values →
  /// `idle`.
  static func state(of status: BroadcastStatus) -> String {
    switch status.state {
    case BroadcastStatus.connecting, "pairing", "started":
      return BroadcastStatus.connecting
    case BroadcastStatus.streaming, BroadcastStatus.reconnecting, BroadcastStatus.failed,
      BroadcastStatus.stopped:
      return status.state
    case "finished":
      return (status.message?.isEmpty ?? true) ? BroadcastStatus.stopped : BroadcastStatus.failed
    default:
      return "idle"
    }
  }

  /// Milliseconds since 1970 of a status timestamp in seconds (0 when it is not a sane value).
  static func milliseconds(_ seconds: Double) -> Int {
    let ms = (seconds * 1000).rounded()
    guard ms.isFinite, ms >= 0, ms < 1e15 else { return 0 }
    return Int(ms)
  }

  /// The map sent to Dart; `message` and `hubName` are left out when empty.
  static func fields(of status: BroadcastStatus) -> [String: AnyHashable] {
    var fields: [String: AnyHashable] = [
      "state": state(of: status),
      "updatedAtMs": milliseconds(status.timestamp),
    ]
    if let message = status.message, !message.isEmpty {
      fields["message"] = message
    }
    if let hubName = status.hubName, !hubName.isEmpty {
      fields["hubName"] = hubName
    }
    return fields
  }

  /// What Dart is told about `status` while the broadcast looks like `sync`
  /// (`BroadcastSync.evaluate`): `fields(of:)` plus `broadcasting` (`sync == .running`).
  ///
  /// A running state (`connecting`, `streaming`, `reconnecting`: the file says the extension
  /// runs) is reported only while `sync` is `.running`, so the contract fields never claim a
  /// broadcast that `broadcasting` denies. When the screen is no longer captured (`.vanished`),
  /// the extension is either writing its final status right now or iOS killed it (memory limit,
  /// crash; the file then keeps its last running state until the vanish check writes `failed`):
  /// the state is `idle`, without the stale `message` and `hubName`.
  static func answer(of status: BroadcastStatus, sync: BroadcastSync) -> [String: AnyHashable] {
    let running = sync == .running
    var answer = fields(of: status)
    if status.isActive && !running {
      answer["state"] = "idle"
      answer["message"] = nil
      answer["hubName"] = nil
    }
    answer["broadcasting"] = running
    answer["timestamp"] = status.timestamp
    return answer
  }

  /// The `broadcastStatus` event for `status` (without `type`), or `nil` while it must be held
  /// back: a running state of an extension that looks `.vanished` is not sent (Dart keeps what
  /// it showed; the final status or the vanish check's `failed` follows within `vanishGrace`).
  /// Otherwise `answer(of:sync:)` without the legacy `timestamp`.
  static func event(of status: BroadcastStatus, sync: BroadcastSync) -> [String: AnyHashable]? {
    if status.isActive && sync == .vanished { return nil }
    var event = answer(of: status, sync: sync)
    event["timestamp"] = nil
    return event
  }

  /// `fields` with plain values for the Flutter codec (`AnyHashable` unwrapped).
  static func channelMap(_ fields: [String: AnyHashable]) -> [String: Any] {
    fields.mapValues { $0.base }
  }
}

/// What the app tells Dart about the broadcast, from the extension's last status and whether the
/// screen is being captured.
enum BroadcastSync: Equatable {
  /// A broadcast runs.
  case running
  /// No broadcast runs; `message` says why the last one ended, if it failed.
  case idle(message: String?)
  /// The status says that the extension runs, but the screen is no longer captured: either the
  /// extension is finishing right now (its final status follows within moments), or
  /// ReplayKit ended it without `broadcastFinished`.
  case vanished

  /// A status that is not an ended state counts as running only while the screen is captured.
  static func evaluate(status: BroadcastStatus?, screenCaptured: Bool) -> BroadcastSync {
    guard let status else { return .idle(message: nil) }
    guard status.isActive else { return .idle(message: status.message) }
    return screenCaptured ? .running : .vanished
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
