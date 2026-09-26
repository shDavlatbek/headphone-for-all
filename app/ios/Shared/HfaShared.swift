// HfaShared.swift - compiled into both the Runner app and the HfaBroadcast extension.
//
// Everything the app and the ReplayKit broadcast upload extension must agree on: the App Group,
// file names inside the shared container, the Darwin notification names and the JSON documents
// exchanged through the container (docs/CONTRACTS.md §8.1, §8.3). Only extension-safe APIs are
// used here (APPLICATION_EXTENSION_API_ONLY).

import Foundation

/// Identifiers and helpers shared by the app and the broadcast extension.
enum HfaShared {
  /// App Group shared by the app and the extension: the `HfaAppGroup` key of both Info.plists,
  /// set from the `HFA_APP_GROUP` build setting (`app/ios/Identity.xcconfig`), which both
  /// entitlements files use too.
  static let appGroupId = infoString("HfaAppGroup") ?? "group.io.github.shdavlatbek.hfa"

  /// Bundle id of the broadcast upload extension (`RPSystemBroadcastPickerView.preferredExtension`):
  /// the `HfaBroadcastExtension` key of both Info.plists (`HFA_BROADCAST_BUNDLE_ID`).
  static let broadcastExtensionBundleId =
    infoString("HfaBroadcastExtension") ?? "io.github.shdavlatbek.hfa.broadcast"

  /// Directory inside the container that holds the Rust data (settings, identity, trusted hubs).
  static let dataDirName = "hfa"

  /// Hub target written by the app (`writeBroadcastConfig`) and read by the extension.
  static let broadcastConfigFileName = "broadcast_config.json"

  /// Last broadcast state written by the extension (read by the app to explain a failure).
  static let broadcastStatusFileName = "broadcast_status.json"

  /// Darwin notification posted by the extension once the Rust sender runs
  /// (`<extension bundle id>.started`: builds with other ids never hear each other).
  static let broadcastStartedNotification = "\(broadcastExtensionBundleId).started"

  /// Darwin notification posted by the extension when the broadcast ends (normally or not).
  static let broadcastFinishedNotification = "\(broadcastExtensionBundleId).finished"

  /// Darwin notification posted by the extension after it wrote a new connection state
  /// (`connecting` / `streaming` / `reconnecting`, with the hub's name) while broadcasting.
  static let broadcastStatusNotification = "\(broadcastExtensionBundleId).status"

  /// A non-empty string value of the current bundle's Info.plist, or `nil` (missing, or a build
  /// setting that was not expanded).
  static func infoString(_ key: String) -> String? {
    guard let value = Bundle.main.object(forInfoDictionaryKey: key) as? String,
      !value.isEmpty, !value.contains("$(")
    else { return nil }
    return value
  }

  /// The App Group container, or `nil` when the App Group entitlement is missing (for example
  /// an unsigned build).
  static func appGroupContainer() -> URL? {
    FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: appGroupId)
  }

  /// `<App Group container>/hfa`, created if needed; `nil` without the App Group.
  static func sharedDataDir() throws -> URL? {
    guard let container = appGroupContainer() else { return nil }
    let dir = container.appendingPathComponent(dataDirName, isDirectory: true)
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    return dir
  }

  /// URL of a file at the top of the App Group container; `nil` without the App Group.
  static func containerFile(_ name: String) -> URL? {
    appGroupContainer()?.appendingPathComponent(name, isDirectory: false)
  }

  /// Posts a Darwin (cross-process) notification. Darwin notifications carry no payload:
  /// details travel through `broadcast_status.json`.
  static func postDarwinNotification(_ name: String) {
    CFNotificationCenterPostNotification(
      CFNotificationCenterGetDarwinNotifyCenter(),
      CFNotificationName(name as CFString),
      nil,
      nil,
      true
    )
  }
}

/// `broadcast_config.json`: the hub the extension streams to. Keys are the snake_case keys of the
/// Rust C ABI configuration (`hfa_ext_sender_start`, docs/CONTRACTS.md §8.5). The extension
/// decodes the file and replaces `data_dir` with the directory it computes itself
/// (`forSender(fileData:dataDir:)`) before passing the JSON to Rust.
struct BroadcastConfig: Codable, Equatable {
  /// Hub host name or address. Required: the app refuses an empty one (`writeBroadcastConfig`),
  /// since the extension cannot look for the hub itself (only the app browses Bonjour).
  var hubHost: String
  /// Hub UDP/TCP port; 0 = the port in the shared settings.
  var hubPort: Int
  /// Hub device id (fingerprint), if known.
  var hubDeviceId: String?
  /// Hub static key (base64url), if known.
  var hubKey: String?
  /// Stream label shown on the hub.
  var label: String
  /// The shared Rust data directory (`<App Group container>/hfa`).
  var dataDir: String

  enum CodingKeys: String, CodingKey {
    case hubHost = "hub_host"
    case hubPort = "hub_port"
    case hubDeviceId = "hub_device_id"
    case hubKey = "hub_key"
    case label
    case dataDir = "data_dir"
  }

  /// Decodes a stored `broadcast_config.json` and points it at `dataDir`, the shared data
  /// directory resolved in the current process.
  ///
  /// The stored `data_dir` is an absolute path recorded when the app wrote the file. Apple does
  /// not guarantee that the App Group container keeps its path (a restore or a migration to a new
  /// device brings the file back with the old container UUID), so it is never trusted.
  ///
  /// - Throws: `DecodingError` when the file is not a valid configuration.
  static func forSender(fileData: Data, dataDir: URL) throws -> BroadcastConfig {
    var config = try JSONDecoder().decode(BroadcastConfig.self, from: fileData)
    config.dataDir = dataDir.path
    return config
  }

  /// Encodes the configuration as JSON (`null` for missing optional values, as Rust expects).
  func jsonData() throws -> Data {
    let encoder = JSONEncoder()
    encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
    return try encoder.encode(self)
  }

  // Explicit encoding so that `nil` becomes `null` instead of a missing key.
  func encode(to encoder: Encoder) throws {
    var c = encoder.container(keyedBy: CodingKeys.self)
    try c.encode(hubHost, forKey: .hubHost)
    try c.encode(hubPort, forKey: .hubPort)
    try c.encode(hubDeviceId, forKey: .hubDeviceId)
    try c.encode(hubKey, forKey: .hubKey)
    try c.encode(label, forKey: .label)
    try c.encode(dataDir, forKey: .dataDir)
  }
}

/// `broadcast_status.json`: written by the extension before it posts a Darwin notification.
///
/// `state` is one of `BroadcastStatus.connecting`, `.streaming`, `.reconnecting` (the extension
/// runs), `.failed`, `.stopped` (it ended). Files written by older builds use `"started"` (runs)
/// and `"finished"` (ended); readers treat any other value like a running broadcast.
struct BroadcastStatus: Codable, Equatable {
  /// The Rust sender runs and connects to (or pairs with) the hub.
  static let connecting = "connecting"
  /// Audio reaches the hub.
  static let streaming = "streaming"
  /// The connection to the hub was lost and is being re-established.
  static let reconnecting = "reconnecting"
  /// The broadcast ended with an error (`message` says why).
  static let failed = "failed"
  /// The broadcast ended normally.
  static let stopped = "stopped"
  /// States of a broadcast that ended (including the legacy `"finished"`).
  static let endedStates: Set<String> = [failed, stopped, "finished"]

  /// See the type's documentation.
  var state: String
  /// Why the broadcast failed (an error description), or `nil`.
  var message: String?
  /// Seconds since 1970 when the status was written.
  var timestamp: Double
  /// Name of the hub the extension streams to, once the hub told it.
  var hubName: String? = nil

  /// Whether the extension was running when it wrote this status (not an ended state).
  var isActive: Bool { !Self.endedStates.contains(state) }

  /// The broadcast state that a Rust sender state (`hfa_ext_sender_state`'s `state`) is
  /// published as while the extension runs; `nil` for the final `failed` / `stopped` (the
  /// extension ends the broadcast and writes those itself) and unknown values.
  static func state(forSenderState senderState: String) -> String? {
    switch senderState {
    case "connecting", "pairing": return connecting
    case "streaming": return streaming
    case "reconnecting": return reconnecting
    default: return nil
    }
  }

  /// Reads the status file; `nil` when it is missing or unreadable.
  static func read() -> BroadcastStatus? {
    guard let url = HfaShared.containerFile(HfaShared.broadcastStatusFileName),
      let data = try? Data(contentsOf: url)
    else { return nil }
    return try? JSONDecoder().decode(BroadcastStatus.self, from: data)
  }

  /// Writes the status file atomically. Returns `false` when there is no App Group container.
  @discardableResult
  func write() -> Bool {
    guard let url = HfaShared.containerFile(HfaShared.broadcastStatusFileName),
      let data = try? JSONEncoder().encode(self)
    else { return false }
    return (try? data.write(to: url, options: .atomic)) != nil
  }
}
