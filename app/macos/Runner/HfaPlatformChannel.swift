// HfaPlatformChannel.swift - macOS side of `MethodChannel('hfa/platform')` (docs/CONTRACTS.md §8.3).
//
// On macOS the Rust core captures system audio itself (Core Audio process taps, macOS 14.2+) and
// plays the hub mix through Core Audio, so the channel only provides the data directory and
// reports whether capture can work on this macOS version. `startHubService` / `stopHubService`
// hold an App Nap opt-out while the hub runs, `beginStreaming` / `endStreaming` while this Mac
// sends; every other method is a no-op.
// The event channel is not registered: Dart listens to it only on Android and iOS.

import FlutterMacOS
import Foundation

/// Handles `hfa/platform` calls on macOS.
///
/// | Method | macOS behaviour |
/// |---|---|
/// | `getDataDir` | `~/Library/Application Support/<bundle id>/hfa` (inside the sandbox container) |
/// | `captureSupport` | `{supported, reason}`: taps need macOS 14.2+ |
/// | `startSystemCapture` | `false` (Rust captures directly) |
/// | `startHubService` / `stopHubService` | begin / end a `ProcessInfo` activity (no App Nap, no idle sleep) |
/// | `beginStreaming` / `endStreaming` | the same for a sender (its own token, independent of the hub's) |
/// | anything else of §8.3 | no-op (`nil`) |
enum HfaPlatformChannel {
  /// Name of the method channel.
  static let methodChannelName = "hfa/platform"

  /// First macOS version with Core Audio process taps (`AudioHardwareCreateProcessTap`).
  static let tapMinimumVersion = OperatingSystemVersion(majorVersion: 14, minorVersion: 2, patchVersion: 0)

  /// Activity token held while the hub runs (main thread only), see `setHubActivity(_:)`.
  private static var hubActivity: NSObjectProtocol?

  /// Activity token held while this Mac sends (main thread only), see `setSenderActivity(_:)`.
  private static var senderActivity: NSObjectProtocol?

  /// Registers the method channel on `messenger`. The handler's only state is the two activity
  /// tokens, so nothing has to be kept alive: the messenger retains the handler block.
  static func register(messenger: FlutterBinaryMessenger) {
    let channel = FlutterMethodChannel(name: methodChannelName, binaryMessenger: messenger)
    channel.setMethodCallHandler { call, result in
      switch call.method {
      case "getDataDir":
        result(dataDirResult())
      case "captureSupport":
        result(captureSupport())
      case "startSystemCapture":
        result(false)
      case "startHubService":
        setHubActivity(true)
        result(nil)
      case "stopHubService":
        setHubActivity(false)
        result(nil)
      case "beginStreaming":
        setSenderActivity(true)
        result(nil)
      case "endStreaming":
        setSenderActivity(false)
        result(nil)
      case "stopSystemCapture", "acquireMulticastLock", "releaseMulticastLock",
        "writeBroadcastConfig":
        result(nil)
      default:
        result(FlutterMethodNotImplemented)
      }
    }
  }

  /// Begins (`true`) or ends the hub's activity assertion; idempotent.
  ///
  /// While the hub runs its window is usually hidden to the tray, and App Nap throttles the timers
  /// and lowers the CPU and I/O priority of an app without visible windows. `.userInitiated`
  /// (which also disables idle system sleep, as audio playback does) and `.latencyCritical` keep
  /// the hub's network, jitter-buffer and mixer threads on time.
  static func setHubActivity(_ active: Bool) {
    if active {
      guard hubActivity == nil else { return }
      hubActivity = ProcessInfo.processInfo.beginActivity(
        options: [.userInitiated, .latencyCritical],
        reason: "Headphone for All hub: receiving and playing audio")
    } else if let activity = hubActivity {
      ProcessInfo.processInfo.endActivity(activity)
      hubActivity = nil
    }
  }

  /// Begins (`true`) or ends the sender's activity assertion; idempotent.
  ///
  /// A Mac that sends usually has its window hidden to the tray while the user works in other
  /// apps, and App Nap would throttle the capture, encoder and network threads of an app
  /// without visible windows (audible as dropouts on the hub). Same options as the hub's.
  static func setSenderActivity(_ active: Bool) {
    if active {
      guard senderActivity == nil else { return }
      senderActivity = ProcessInfo.processInfo.beginActivity(
        options: [.userInitiated, .latencyCritical],
        reason: "Headphone for All: sending this Mac's audio")
    } else if let activity = senderActivity {
      ProcessInfo.processInfo.endActivity(activity)
      senderActivity = nil
    }
  }

  /// Application Support `/hfa`, created if needed. Under the App Sandbox Application Support
  /// resolves inside the app's container. The bundle id sub-directory keeps it apart from other
  /// apps when the sandbox is off.
  private static func dataDirResult() -> Any {
    do {
      let support = try FileManager.default.url(
        for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
      let base = Bundle.main.bundleIdentifier.map {
        support.appendingPathComponent($0, isDirectory: true)
      } ?? support
      let dir = base.appendingPathComponent("hfa", isDirectory: true)
      try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
      return dir.path
    } catch {
      return FlutterError(
        code: "DATA_DIR", message: "cannot create the data directory: \(error.localizedDescription)",
        details: nil)
    }
  }

  /// Whether system-audio capture can work here (the Rust side checks again and returns a clear
  /// error on older systems).
  private static func captureSupport() -> [String: Any] {
    if ProcessInfo.processInfo.isOperatingSystemAtLeast(tapMinimumVersion) {
      return ["supported": true, "reason": "processTap"]
    }
    let v = ProcessInfo.processInfo.operatingSystemVersion
    return [
      "supported": false,
      "reason":
        "Capturing this Mac's audio needs macOS 14.2 or later (this Mac runs \(v.majorVersion).\(v.minorVersion).\(v.patchVersion)). It can still be the hub.",
    ]
  }
}
