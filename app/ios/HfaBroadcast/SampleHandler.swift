// SampleHandler.swift - the ReplayKit broadcast upload extension "HfaBroadcast".
//
// The user starts it from the app's broadcast picker (or Control Center). ReplayKit then hands
// every captured sample buffer to `processSampleBuffer`. Only `.audioApp` (what the phone
// plays) is used: it is converted to interleaved Float32 and pushed into the Rust sender
// (`hfa_ext_*`, core/hfa-ffi/include/hfa_ext.h), which encodes it with Opus and streams it to
// the paired hub. Video and microphone buffers are ignored.
//
// The extension runs in its own process with a ~50 MB memory cap, no Flutter and no pairing:
// it reads `broadcast_config.json` (written by the app) from the App Group container and uses
// the identity and trusted hubs the app stored in `<container>/hfa`.

import CoreMedia
import Foundation
import ReplayKit
import os

/// Principal class of the broadcast upload extension (`NSExtensionPrincipalClass`).
final class SampleHandler: RPBroadcastSampleHandler {
  private static let log = Logger(subsystem: HfaShared.broadcastExtensionBundleId, category: "broadcast")
  /// NSError domain of the errors shown to the user when the broadcast cannot start.
  static let errorDomain = HfaShared.broadcastExtensionBundleId

  /// Error codes of `errorDomain`.
  enum ErrorCode: Int {
    case noAppGroup = 1
    case noConfig = 2
    case senderFailed = 3
  }

  /// Guards `sender`: ReplayKit may call `processSampleBuffer` and `broadcastFinished` on
  /// different threads, and a handle must not be used concurrently or after it was freed.
  private let lock = NSLock()
  private var sender: OpaquePointer?
  private let interleaver = PcmInterleaver()
  /// Failures already logged (a bad format would otherwise log for every buffer).
  private var loggedFailures = Set<String>()
  private var loggedPushError = false
  /// Polls `hfa_ext_sender_state` while the sender runs (guarded by `lock`).
  private var stateTimer: DispatchSourceTimer?
  /// Queue of `stateTimer`; `lastSenderState` is only used on it.
  private let stateQueue = DispatchQueue(label: "\(HfaShared.broadcastExtensionBundleId).state")
  private var lastSenderState: String?
  /// How often the sender state is polled.
  private static let statePollInterval = DispatchTimeInterval.seconds(1)

  // MARK: - Broadcast lifecycle

  override func broadcastStarted(withSetupInfo setupInfo: [String: NSObject]?) {
    let started: Result<OpaquePointer, NSError> = startSender()
    switch started {
    case let .success(handle):
      lock.lock()
      sender = handle
      lock.unlock()
      startStatePolling()
      Self.log.info("broadcast started")
      BroadcastStatus(state: "started", message: nil, timestamp: Date().timeIntervalSince1970)
        .write()
      HfaShared.postDarwinNotification(HfaShared.broadcastStartedNotification)
    case let .failure(error):
      Self.log.error("cannot start: \(error.localizedDescription, privacy: .public)")
      BroadcastStatus(
        state: "finished", message: error.localizedDescription,
        timestamp: Date().timeIntervalSince1970
      ).write()
      HfaShared.postDarwinNotification(HfaShared.broadcastFinishedNotification)
      finishBroadcastWithError(error)
    }
  }

  override func broadcastPaused() {
    // Nothing to do: ReplayKit stops delivering buffers; the sender keeps its connection.
  }

  override func broadcastResumed() {}

  override func broadcastFinished() {
    // After a failed start `finishBroadcastWithError` already reported the reason; do not
    // overwrite that status with a plain "finished".
    guard stopSender() else { return }
    Self.log.info("broadcast finished")
    BroadcastStatus(state: "finished", message: nil, timestamp: Date().timeIntervalSince1970)
      .write()
    HfaShared.postDarwinNotification(HfaShared.broadcastFinishedNotification)
  }

  override func processSampleBuffer(
    _ sampleBuffer: CMSampleBuffer, with sampleBufferType: RPSampleBufferType
  ) {
    switch sampleBufferType {
    case .audioApp:
      pushAppAudio(sampleBuffer)
    case .video, .audioMic:
      break
    @unknown default:
      break
    }
  }

  // MARK: - Rust sender

  /// Reads the configuration written by the app and starts the Rust sender.
  private func startSender() -> Result<OpaquePointer, NSError> {
    let noAppGroup = Self.error(
      .noAppGroup,
      "Headphone for All cannot reach its shared storage (App Group \(HfaShared.appGroupId)). Reinstall the app."
    )
    // The data directory is resolved here, never taken from the file: the container path the
    // app recorded can be stale after a restore or a device migration.
    let dataDir: URL
    do {
      guard let dir = try HfaShared.sharedDataDir() else { return .failure(noAppGroup) }
      dataDir = dir
    } catch {
      Self.log.error("cannot create the data directory: \(error.localizedDescription, privacy: .public)")
      return .failure(noAppGroup)
    }
    guard let url = HfaShared.containerFile(HfaShared.broadcastConfigFileName),
      let data = try? Data(contentsOf: url),
      let config = try? BroadcastConfig.forSender(fileData: data, dataDir: dataDir),
      let configJSON = try? config.jsonData()
    else {
      return .failure(
        Self.error(
          .noConfig,
          "Open Headphone for All, pair this iPhone with your headphone hub and choose it as the target, then start the broadcast again."
        ))
    }
    let json = String(decoding: configJSON, as: UTF8.self)
    let handle = json.withCString { hfa_ext_sender_start($0) }
    guard let handle else {
      // Only local problems fail here (config, pairing, storage): the connection to the hub is
      // made in the background, so an unreachable hub does not stop the broadcast.
      let reason = Self.lastRustError() ?? "unknown error"
      return .failure(
        Self.error(
          .senderFailed,
          "Could not start sending audio to the headphone hub: \(reason). Pair this iPhone with the hub again in the Headphone for All app."
        ))
    }
    return .success(handle)
  }

  /// Stops and frees the Rust sender (idempotent). Returns whether a sender was running.
  @discardableResult
  private func stopSender() -> Bool {
    lock.lock()
    let handle = sender
    sender = nil
    let timer = stateTimer
    stateTimer = nil
    lock.unlock()
    timer?.cancel()
    guard let handle else { return false }
    if hfa_ext_sender_stop(handle) != 0 {
      Self.log.error(
        "stopping the sender failed: \(Self.lastRustError() ?? "unknown error", privacy: .public)")
    }
    return true
  }

  /// Converts one `.audioApp` buffer and pushes it into the sender.
  private func pushAppAudio(_ sampleBuffer: CMSampleBuffer) {
    lock.lock()
    defer { lock.unlock() }
    guard let handle = sender else { return }
    switch interleaver.convert(sampleBuffer) {
    case let .success(output):
      guard output.frames > 0 else { return }
      let status = hfa_ext_push_pcm(
        handle, output.samples, UInt32(output.frames), UInt32(output.channels),
        UInt32(output.sampleRate))
      if status != 0, !loggedPushError {
        loggedPushError = true
        Self.log.error(
          "hfa_ext_push_pcm failed (\(status)): \(Self.lastRustError() ?? "unknown error", privacy: .public)"
        )
      }
    case let .failure(failure):
      let text = failure.description
      if loggedFailures.insert(text).inserted {
        Self.log.error("dropping audio: \(text, privacy: .public)")
      }
    }
  }

  // MARK: - Sender state

  /// The fields of the `hfa_ext_sender_state` document the extension uses.
  private struct SenderStateInfo: Decodable {
    let state: String
    let error: String?
  }

  /// The connection to the hub is made in the background after `hfa_ext_sender_start`, and a
  /// refusal by the hub (pairing required, key mismatch) is final. Without polling, the user
  /// would see a running broadcast that never plays.
  private func startStatePolling() {
    let timer = DispatchSource.makeTimerSource(queue: stateQueue)
    timer.schedule(
      deadline: .now() + Self.statePollInterval, repeating: Self.statePollInterval,
      leeway: .milliseconds(200))
    timer.setEventHandler { [weak self] in self?.pollSenderState() }
    lock.lock()
    // The broadcast may already have finished (stopSender ran): then do not poll at all.
    guard sender != nil else {
      lock.unlock()
      return
    }
    stateTimer = timer
    lock.unlock()
    timer.resume()
  }

  /// Reads the sender state; ends the broadcast with the reason once the sender failed.
  private func pollSenderState() {
    lock.lock()
    guard let handle = sender else {
      lock.unlock()
      return
    }
    let json = Self.readSenderState(handle)
    lock.unlock()
    guard let json, let info = try? JSONDecoder().decode(SenderStateInfo.self, from: Data(json.utf8))
    else { return }
    if info.state != lastSenderState {
      lastSenderState = info.state
      Self.log.info("sender state: \(info.state, privacy: .public)")
    }
    if info.state == "failed" {
      failBroadcast(reason: info.error ?? "unknown error")
    }
  }

  /// `hfa_ext_sender_state` as a string (the buffer grows once if the document is larger).
  private static func readSenderState(_ handle: OpaquePointer) -> String? {
    var buffer = [CChar](repeating: 0, count: 512)
    for _ in 0..<2 {
      let count = buffer.count
      let written = buffer.withUnsafeMutableBufferPointer {
        hfa_ext_sender_state(handle, $0.baseAddress, UInt32(count))
      }
      if written < 0 { return nil }
      if Int(written) < count {
        return String(
          decoding: buffer.prefix(Int(written)).map { UInt8(bitPattern: $0) }, as: UTF8.self)
      }
      buffer = [CChar](repeating: 0, count: Int(written) + 1)
    }
    return nil
  }

  /// Stops the sender and ends the broadcast with `reason` (once).
  private func failBroadcast(reason: String) {
    guard stopSender() else { return }
    let error = Self.error(
      .senderFailed,
      "The headphone hub did not accept this iPhone (\(reason)). Pair this iPhone with the hub again in the Headphone for All app, then start the broadcast again."
    )
    Self.log.error("sender failed: \(reason, privacy: .public)")
    BroadcastStatus(
      state: "finished", message: error.localizedDescription,
      timestamp: Date().timeIntervalSince1970
    ).write()
    HfaShared.postDarwinNotification(HfaShared.broadcastFinishedNotification)
    finishBroadcastWithError(error)
  }

  // MARK: - Helpers

  /// Message of the last failed `hfa_ext_*` call on this thread.
  private static func lastRustError() -> String? {
    guard let message = hfa_ext_last_error() else { return nil }
    return String(cString: message)
  }

  private static func error(_ code: ErrorCode, _ message: String) -> NSError {
    NSError(
      domain: errorDomain, code: code.rawValue,
      userInfo: [NSLocalizedDescriptionKey: message])
  }
}
