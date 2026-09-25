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

  // MARK: - Broadcast lifecycle

  override func broadcastStarted(withSetupInfo setupInfo: [String: NSObject]?) {
    let started: Result<OpaquePointer, NSError> = startSender()
    switch started {
    case let .success(handle):
      lock.lock()
      sender = handle
      lock.unlock()
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
    guard HfaShared.appGroupContainer() != nil else {
      return .failure(
        Self.error(
          .noAppGroup,
          "Headphone for All cannot reach its shared storage (App Group \(HfaShared.appGroupId)). Reinstall the app."
        ))
    }
    guard let url = HfaShared.containerFile(HfaShared.broadcastConfigFileName),
      let data = try? Data(contentsOf: url),
      let json = String(data: data, encoding: .utf8)
    else {
      return .failure(
        Self.error(
          .noConfig,
          "Open Headphone for All, pair this iPhone with your headphone hub and choose it as the target, then start the broadcast again."
        ))
    }
    // The file already uses the keys of the C ABI configuration (see BroadcastConfig).
    let handle = json.withCString { hfa_ext_sender_start($0) }
    guard let handle else {
      let reason = Self.lastRustError() ?? "unknown error"
      return .failure(
        Self.error(
          .senderFailed,
          "Could not start sending audio to the headphone hub: \(reason). Check that the hub is running and paired in the Headphone for All app."
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
    lock.unlock()
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
