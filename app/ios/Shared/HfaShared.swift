// HfaShared.swift - compiled into both the Runner app and the HfaBroadcast extension.
//
// Everything the app and the ReplayKit broadcast upload extension must agree on: the App Group,
// file names inside the shared container, the Darwin notification names and the JSON documents
// exchanged through the container (docs/CONTRACTS.md §8.1, §8.3). Only extension-safe APIs are
// used here (APPLICATION_EXTENSION_API_ONLY).

import Foundation

/// Identifiers and helpers shared by the app and the broadcast extension.
enum HfaShared {
  /// App Group shared by the app and the extension (both entitlements files list it).
  static let appGroupId = "group.io.github.shdavlatbek.hfa"

  /// Bundle id of the broadcast upload extension (`RPSystemBroadcastPickerView.preferredExtension`).
  static let broadcastExtensionBundleId = "io.github.shdavlatbek.hfa.broadcast"

  /// Directory inside the container that holds the Rust data (settings, identity, trusted hubs).
  static let dataDirName = "hfa"

  /// Hub target written by the app (`writeBroadcastConfig`) and read by the extension.
  static let broadcastConfigFileName = "broadcast_config.json"

  /// Last broadcast state written by the extension (read by the app to explain a failure).
  static let broadcastStatusFileName = "broadcast_status.json"

  /// Darwin notification posted by the extension once the Rust sender runs.
  static let broadcastStartedNotification = "io.github.shdavlatbek.hfa.broadcast.started"

  /// Darwin notification posted by the extension when the broadcast ends (normally or not).
  static let broadcastFinishedNotification = "io.github.shdavlatbek.hfa.broadcast.finished"

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
  /// Hub host name or address; empty = find `hubDeviceId` over mDNS.
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
struct BroadcastStatus: Codable, Equatable {
  /// `"started"` or `"finished"`.
  var state: String
  /// Why the broadcast finished (an error description), or `nil`.
  var message: String?
  /// Seconds since 1970 when the status was written.
  var timestamp: Double

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
