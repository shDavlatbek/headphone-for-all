// RunnerTests.swift - unit tests of the iOS native code that can run without a device:
// the data directory policy of the platform channel, the App Group file formats
// (HfaShared.swift, compiled into Runner), the broadcast status reported to Dart, the Bonjour
// discovery backend's codecs and its registration with Rust, and the ReplayKit PCM
// conversion of the broadcast extension (PcmInterleaver.swift is compiled into this test target
// too, since an app extension cannot be a test host).
//
// Run on a Mac: xcodebuild test -workspace ios/Runner.xcworkspace -scheme Runner \
//   -destination 'platform=iOS Simulator,name=iPhone 16'   (after `flutter build ios --simulator`)

import AudioToolbox
import CoreMedia
import Flutter
import Foundation
import XCTest

@testable import Runner

final class DataDirTests: XCTestCase {
  private struct Failure: Error {}

  func testUsesTheAppGroupDirectory() {
    let dir = URL(fileURLWithPath: "/private/var/group/hfa")
    let result = HfaPlatformChannel.resolveDataDir(
      shared: { dir },
      privateFallback: {
        XCTFail("the fallback must not be used when the App Group exists")
        return dir
      })
    XCTAssertEqual(result as? String, dir.path)
  }

  /// A device build without the App Group must not start on a private directory (the broadcast
  /// extension could not see its pairings): the Dart bootstrap turns this into a start-up error.
  func testMissingAppGroupIsAnError() {
    let result = HfaPlatformChannel.resolveDataDir(shared: { nil }, privateFallback: nil)
    let error = result as? FlutterError
    XCTAssertEqual(error?.code, "NO_APP_GROUP")
    XCTAssertEqual(error?.message, HfaPlatformChannel.noAppGroupMessage)
    XCTAssertTrue(HfaPlatformChannel.noAppGroupMessage.contains(HfaShared.appGroupId))
  }

  func testPrivateFallbackWhenAllowed() {
    let result = HfaPlatformChannel.resolveDataDir(
      shared: { nil }, privateFallback: { URL(fileURLWithPath: "/support/hfa") })
    XCTAssertEqual(result as? String, "/support/hfa")
  }

  func testIoFailureIsDataDirError() {
    let result = HfaPlatformChannel.resolveDataDir(shared: { throw Failure() }, privateFallback: nil)
    XCTAssertEqual((result as? FlutterError)?.code, "DATA_DIR")
  }

  func testPrivateFallbackOnlyInTheSimulator() {
    #if targetEnvironment(simulator)
      XCTAssertTrue(HfaPlatformChannel.allowsPrivateDataDir)
    #else
      XCTAssertFalse(HfaPlatformChannel.allowsPrivateDataDir)
    #endif
  }
}

final class BroadcastConfigTests: XCTestCase {
  func testEncodesTheRustConfigKeysWithNulls() throws {
    let config = BroadcastConfig(
      hubHost: "192.168.1.20", hubPort: 47_810, hubDeviceId: "ab12-cd34", hubKey: nil,
      label: "iPhone", dataDir: "/private/var/group/hfa")
    let object = try JSONSerialization.jsonObject(with: config.jsonData())
    let json = try XCTUnwrap(object as? [String: Any])
    XCTAssertEqual(
      Set(json.keys), ["hub_host", "hub_port", "hub_device_id", "hub_key", "label", "data_dir"])
    XCTAssertEqual(json["hub_host"] as? String, "192.168.1.20")
    XCTAssertEqual(json["hub_port"] as? Int, 47_810)
    XCTAssertEqual(json["hub_device_id"] as? String, "ab12-cd34")
    XCTAssertTrue(json["hub_key"] is NSNull, "a missing key must be written as null")
    XCTAssertEqual(json["data_dir"] as? String, "/private/var/group/hfa")
  }

  func testRoundTrips() throws {
    let config = BroadcastConfig(
      hubHost: "", hubPort: 0, hubDeviceId: "id", hubKey: "a2V5", label: "", dataDir: "/d")
    let decoded = try JSONDecoder().decode(BroadcastConfig.self, from: config.jsonData())
    XCTAssertEqual(decoded, config)
  }

  func testSenderConfigIgnoresTheStoredDataDir() throws {
    // A file restored from a backup still names the old container UUID.
    let stored = Data(
      #"{"data_dir":"/private/var/mobile/Containers/Shared/AppGroup/OLD-UUID/hfa","hub_device_id":"ab12-cd34","hub_host":"10.0.0.2","hub_key":null,"hub_port":47810,"label":"iPhone"}"#
        .utf8)
    let current = URL(fileURLWithPath: "/private/var/mobile/Containers/Shared/AppGroup/NEW-UUID/hfa")
    let config = try BroadcastConfig.forSender(fileData: stored, dataDir: current)
    XCTAssertEqual(
      config,
      BroadcastConfig(
        hubHost: "10.0.0.2", hubPort: 47_810, hubDeviceId: "ab12-cd34", hubKey: nil,
        label: "iPhone", dataDir: current.path))
    let object = try JSONSerialization.jsonObject(with: config.jsonData())
    let json = try XCTUnwrap(object as? [String: Any])
    XCTAssertEqual(json["data_dir"] as? String, current.path)
  }

  func testSenderConfigRejectsAnInvalidFile() {
    let dir = URL(fileURLWithPath: "/d")
    XCTAssertThrowsError(try BroadcastConfig.forSender(fileData: Data("{}".utf8), dataDir: dir))
    XCTAssertThrowsError(try BroadcastConfig.forSender(fileData: Data("not json".utf8), dataDir: dir))
  }

  func testStatusRoundTrips() throws {
    let status = BroadcastStatus(state: "finished", message: "hub not paired", timestamp: 12.5)
    let data = try JSONEncoder().encode(status)
    XCTAssertEqual(try JSONDecoder().decode(BroadcastStatus.self, from: data), status)
  }
}

final class BroadcastSyncTests: XCTestCase {
  private func status(_ state: String, _ message: String? = nil) -> BroadcastStatus {
    BroadcastStatus(state: state, message: message, timestamp: 1_000)
  }

  func testNoStatusMeansIdle() {
    XCTAssertEqual(BroadcastSync.evaluate(status: nil, screenCaptured: true), .idle(message: nil))
  }

  func testFinishedCarriesItsReason() {
    XCTAssertEqual(
      BroadcastSync.evaluate(status: status("finished", "hub not paired"), screenCaptured: false),
      .idle(message: "hub not paired"))
    // Someone records the screen: still no broadcast of ours.
    XCTAssertEqual(
      BroadcastSync.evaluate(status: status("finished"), screenCaptured: true),
      .idle(message: nil))
  }

  /// The app was relaunched while the extension kept broadcasting.
  func testStartedWhileCapturedIsRunning() {
    XCTAssertEqual(BroadcastSync.evaluate(status: status("started"), screenCaptured: true), .running)
    XCTAssertEqual(
      BroadcastSync.evaluate(status: status("streaming"), screenCaptured: true), .running,
      "unknown states count as running")
  }

  /// ReplayKit ended the extension without `broadcastFinished` (memory limit, crash).
  func testStartedWithoutCaptureHasVanished() {
    XCTAssertEqual(BroadcastSync.evaluate(status: status("started"), screenCaptured: false), .vanished)
  }

  func testStatusIsActiveUntilFinished() {
    XCTAssertTrue(status("started").isActive)
    XCTAssertFalse(status("finished").isActive)
    for running in ["connecting", "streaming", "reconnecting"] {
      XCTAssertTrue(status(running).isActive, running)
    }
    XCTAssertFalse(status("failed").isActive)
    XCTAssertFalse(status("stopped").isActive)
  }

  func testFailedAndStoppedAreIdle() {
    XCTAssertEqual(
      BroadcastSync.evaluate(status: status("failed", "hub refused"), screenCaptured: true),
      .idle(message: "hub refused"))
    XCTAssertEqual(
      BroadcastSync.evaluate(status: status("stopped"), screenCaptured: false), .idle(message: nil))
    XCTAssertEqual(
      BroadcastSync.evaluate(status: status("reconnecting"), screenCaptured: false), .vanished)
  }
}

final class BroadcastStatusReportTests: XCTestCase {
  private func status(_ state: String, _ message: String? = nil, hub: String? = nil)
    -> BroadcastStatus
  {
    BroadcastStatus(state: state, message: message, timestamp: 1_700_000_000.1234, hubName: hub)
  }

  func testStatesFollowTheExtension() {
    for state in ["connecting", "streaming", "reconnecting", "failed", "stopped"] {
      XCTAssertEqual(BroadcastStatusReport.state(of: status(state)), state)
    }
    XCTAssertEqual(BroadcastStatusReport.state(of: status("pairing")), "connecting")
  }

  /// Files written by older builds.
  func testLegacyStates() {
    XCTAssertEqual(BroadcastStatusReport.state(of: status("started")), "connecting")
    XCTAssertEqual(BroadcastStatusReport.state(of: status("finished")), "stopped")
    XCTAssertEqual(BroadcastStatusReport.state(of: status("finished", "")), "stopped")
    XCTAssertEqual(BroadcastStatusReport.state(of: status("finished", "not paired")), "failed")
    XCTAssertEqual(BroadcastStatusReport.state(of: status("something new")), "idle")
    let legacy = Data(#"{"state":"finished","message":"boom","timestamp":12.5}"#.utf8)
    let decoded = try? JSONDecoder().decode(BroadcastStatus.self, from: legacy)
    XCTAssertEqual(decoded, BroadcastStatus(state: "finished", message: "boom", timestamp: 12.5))
    XCTAssertNil(decoded?.hubName)
  }

  func testEveryReportedStateIsInTheContract() {
    for state in ["connecting", "pairing", "streaming", "reconnecting", "failed", "stopped",
      "started", "finished", "x"]
    {
      XCTAssertTrue(
        BroadcastStatusReport.states.contains(BroadcastStatusReport.state(of: status(state))),
        state)
    }
  }

  func testFields() {
    let fields = BroadcastStatusReport.fields(of: status("streaming", hub: "Desk"))
    XCTAssertEqual(
      fields, ["state": "streaming", "hubName": "Desk", "updatedAtMs": 1_700_000_000_123])
    let failed = BroadcastStatusReport.fields(of: status("failed", "hub refused", hub: ""))
    XCTAssertEqual(
      failed, ["state": "failed", "message": "hub refused", "updatedAtMs": 1_700_000_000_123])
    let map = BroadcastStatusReport.channelMap(failed)
    XCTAssertEqual(map["updatedAtMs"] as? Int, 1_700_000_000_123)
    XCTAssertEqual(map["message"] as? String, "hub refused")
    XCTAssertNil(map["hubName"])
  }

  /// A running broadcast: the contract fields plus the legacy keys.
  func testAnswerWhileRunning() {
    let streaming = status("streaming", hub: "Desk")
    XCTAssertEqual(
      BroadcastStatusReport.answer(of: streaming, sync: .running),
      [
        "state": "streaming", "hubName": "Desk", "updatedAtMs": 1_700_000_000_123,
        "broadcasting": true, "timestamp": 1_700_000_000.1234,
      ])
    XCTAssertEqual(
      BroadcastStatusReport.event(of: streaming, sync: .running),
      ["state": "streaming", "hubName": "Desk", "updatedAtMs": 1_700_000_000_123, "broadcasting": true])
  }

  /// iOS killed the extension while the app was not running: the file still says `streaming`,
  /// but the screen is no longer captured. Dart must not see a running state (its parser only
  /// downgrades one when it also reads `broadcasting: false`).
  func testVanishedExtensionIsNotReportedAsRunning() {
    let stale = status("streaming", "old hint", hub: "Desk")
    let sync = BroadcastSync.evaluate(status: stale, screenCaptured: false)
    XCTAssertEqual(sync, .vanished)
    let answer = BroadcastStatusReport.answer(of: stale, sync: sync)
    XCTAssertEqual(
      answer,
      [
        "state": "idle", "updatedAtMs": 1_700_000_000_123, "broadcasting": false,
        "timestamp": 1_700_000_000.1234,
      ])
    XCTAssertNil(
      BroadcastStatusReport.event(of: stale, sync: sync),
      "the event waits for the final status or the vanish check's failed")
    for running in ["connecting", "reconnecting", "started", "pairing"] {
      let file = status(running)
      let answer = BroadcastStatusReport.answer(of: file, sync: .vanished)
      XCTAssertEqual(answer["state"], AnyHashable("idle"), running)
      XCTAssertNil(BroadcastStatusReport.event(of: file, sync: .vanished), running)
    }
  }

  /// An ended broadcast is reported as it is, whatever the screen capture says.
  func testEndedBroadcastIsAlwaysReported() {
    let failed = status("failed", "hub refused", hub: "Desk")
    for captured in [false, true] {
      let sync = BroadcastSync.evaluate(status: failed, screenCaptured: captured)
      XCTAssertEqual(
        BroadcastStatusReport.event(of: failed, sync: sync),
        [
          "state": "failed", "message": "hub refused", "hubName": "Desk",
          "updatedAtMs": 1_700_000_000_123, "broadcasting": false,
        ])
      XCTAssertEqual(
        BroadcastStatusReport.answer(of: failed, sync: sync)["state"], AnyHashable("failed"))
    }
  }

  /// Every state reported with `broadcasting: false` is one that does not claim a broadcast.
  func testRunningStateOnlyWithBroadcasting() {
    let running: Set<AnyHashable> = ["connecting", "streaming", "reconnecting"]
    for state in ["connecting", "pairing", "streaming", "reconnecting", "failed", "stopped",
      "started", "finished", "x"]
    {
      for captured in [false, true] {
        let file = status(state)
        let sync = BroadcastSync.evaluate(status: file, screenCaptured: captured)
        let answer = BroadcastStatusReport.answer(of: file, sync: sync)
        if let reported = answer["state"], running.contains(reported) {
          XCTAssertEqual(answer["broadcasting"], AnyHashable(true), "\(state), captured \(captured)")
        }
        if let event = BroadcastStatusReport.event(of: file, sync: sync),
          let reported = event["state"], running.contains(reported)
        {
          XCTAssertEqual(event["broadcasting"], AnyHashable(true), "\(state), captured \(captured)")
        }
      }
    }
  }

  func testMillisecondsAreSane() {
    XCTAssertEqual(BroadcastStatusReport.milliseconds(1.5), 1_500)
    XCTAssertEqual(BroadcastStatusReport.milliseconds(.nan), 0)
    XCTAssertEqual(BroadcastStatusReport.milliseconds(.infinity), 0)
    XCTAssertEqual(BroadcastStatusReport.milliseconds(-3), 0)
    XCTAssertEqual(BroadcastStatusReport.milliseconds(1e300), 0)
  }

  func testSenderStatesMapToBroadcastStates() {
    XCTAssertEqual(BroadcastStatus.state(forSenderState: "connecting"), "connecting")
    XCTAssertEqual(BroadcastStatus.state(forSenderState: "pairing"), "connecting")
    XCTAssertEqual(BroadcastStatus.state(forSenderState: "streaming"), "streaming")
    XCTAssertEqual(BroadcastStatus.state(forSenderState: "reconnecting"), "reconnecting")
    XCTAssertNil(BroadcastStatus.state(forSenderState: "failed"))
    XCTAssertNil(BroadcastStatus.state(forSenderState: "stopped"))
  }

  func testStatusWithHubNameRoundTrips() throws {
    let original = status("streaming", hub: "Desk")
    let data = try JSONEncoder().encode(original)
    XCTAssertEqual(try JSONDecoder().decode(BroadcastStatus.self, from: data), original)
  }
}

final class BonjourCodecTests: XCTestCase {
  private typealias Entry = HfaBonjourCodec.TXTEntry

  func testTXTRecordRoundTrips() {
    let entries = [
      Entry(key: "v", value: "0"), Entry(key: "id", value: "ab12-cd34"),
      Entry(key: "name", value: "Küche = Desk"), Entry(key: "platform", value: "ios"),
    ]
    let data = HfaBonjourCodec.txtRecordData(entries)
    XCTAssertEqual(Array(data.prefix(4)), [3, UInt8(ascii: "v"), UInt8(ascii: "="), UInt8(ascii: "0")])
    XCTAssertEqual(
      HfaBonjourCodec.parseTXTRecord(data),
      ["v": "0", "id": "ab12-cd34", "name": "Küche = Desk", "platform": "ios"])
  }

  func testTXTRecordSkipsInvalidEntries() {
    let long = String(repeating: "x", count: 254)
    let data = HfaBonjourCodec.txtRecordData([
      Entry(key: "", value: "a"), Entry(key: "a=b", value: "c"), Entry(key: "n", value: long),
      Entry(key: "ok", value: "1"),
    ])
    XCTAssertEqual(HfaBonjourCodec.parseTXTRecord(data), ["ok": "1"])
    XCTAssertEqual(HfaBonjourCodec.txtRecordData([]), Data([0]), "an empty TXT record is one empty string")
    let fits = HfaBonjourCodec.txtRecordData([Entry(key: "n", value: String(long.dropLast()))])
    XCTAssertEqual(fits.count, 256)
  }

  func testTXTParsingRules() {
    var bytes: [UInt8] = []
    for entry in ["ID=first", "id=second", "flag", "=novalue", "name="] {
      bytes.append(UInt8(entry.utf8.count))
      bytes.append(contentsOf: Array(entry.utf8))
    }
    bytes += [2, 0xFF, 0xFE]  // not UTF-8
    bytes += [9, UInt8(ascii: "x")]  // truncated
    XCTAssertEqual(
      HfaBonjourCodec.parseTXTRecord(Data(bytes)), ["id": "first", "flag": "", "name": ""])
    XCTAssertEqual(HfaBonjourCodec.parseTXTRecord(Data()), [:])
    XCTAssertEqual(HfaBonjourCodec.parseTXTRecord(Data([0])), [:])
    XCTAssertEqual(HfaBonjourCodec.normalizedTXT(["ID": "a", "id": "b", "Name": "c"]), ["id": "a", "name": "c"])
  }

  /// `HfaBonjourCodec.ipString` of a socket address holding `text` (IPv4 or IPv6).
  private func ipString(of text: String) -> String? {
    if text.contains(":") {
      var address = sockaddr_in6()
      address.sin6_family = sa_family_t(AF_INET6)
      address.sin6_len = UInt8(MemoryLayout<sockaddr_in6>.size)
      XCTAssertEqual(inet_pton(AF_INET6, text, &address.sin6_addr), 1, text)
      return withUnsafePointer(to: &address) { pointer in
        pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { HfaBonjourCodec.ipString($0) }
      }
    }
    var address = sockaddr_in()
    address.sin_family = sa_family_t(AF_INET)
    address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
    XCTAssertEqual(inet_pton(AF_INET, text, &address.sin_addr), 1, text)
    return withUnsafePointer(to: &address) { pointer in
      pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { HfaBonjourCodec.ipString($0) }
    }
  }

  func testSocketAddressesBecomeNumericHosts() {
    XCTAssertEqual(ipString(of: "192.168.1.20"), "192.168.1.20")
    XCTAssertEqual(ipString(of: "fd00::20"), "fd00::20")
    XCTAssertNil(ipString(of: "fe80::1"), "link-local IPv6 is useless without its zone")
    var unix = sockaddr()
    unix.sa_family = sa_family_t(AF_UNIX)
    XCTAssertNil(withUnsafePointer(to: &unix) { HfaBonjourCodec.ipString($0) })
  }

  func testResolvedJSONForRust() throws {
    let json = try XCTUnwrap(
      HfaBonjourCodec.resolvedJSON(
        instance: "Desk (ab12)", txt: ["v": "0"], addresses: ["192.168.1.20", "fd00::20"],
        port: 47_810))
    let object = try XCTUnwrap(
      JSONSerialization.jsonObject(with: Data(json.utf8)) as? [String: Any])
    XCTAssertEqual(object["instance"] as? String, "Desk (ab12)")
    XCTAssertEqual(object["txt"] as? [String: String], ["v": "0"])
    XCTAssertEqual(object["addrs"] as? [String], ["192.168.1.20", "fd00::20"])
    XCTAssertEqual(object["port"] as? Int, 47_810)
  }

  func testAdvertFromRust() {
    let json = #"{"instance":"Desk (ab12)","type":"_hfa._tcp","domain":"local.","port":47810,"txt":[["v","0"],["name","Desk"]]}"#
    XCTAssertEqual(
      HfaBonjourCodec.decodeAdvert(json),
      HfaBonjourCodec.Advert(
        instance: "Desk (ab12)", type: "_hfa._tcp", domain: "local.", port: 47_810,
        txt: [Entry(key: "v", value: "0"), Entry(key: "name", value: "Desk")]))
    for bad in [
      "not json",
      #"{"instance":"a","type":"_hfa._tcp","domain":"local.","port":0,"txt":[]}"#,
      #"{"instance":"a","type":"_hfa._tcp","domain":"local.","port":70000,"txt":[]}"#,
      #"{"instance":"","type":"_hfa._tcp","domain":"local.","port":1,"txt":[]}"#,
      #"{"instance":"a","type":"_hfa._tcp","domain":"local.","port":1,"txt":[["v"]]}"#,
    ] {
      XCTAssertNil(HfaBonjourCodec.decodeAdvert(bad), bad)
    }
  }

  func testServiceTypeIsDeclaredInInfoPlist() {
    let services = Bundle.main.object(forInfoDictionaryKey: "NSBonjourServices") as? [String]
    XCTAssertEqual(services, [HfaBonjourCodec.serviceType])
    XCTAssertNotNil(Bundle.main.object(forInfoDictionaryKey: "NSLocalNetworkUsageDescription"))
  }
}

/// The app registers its Bonjour backend with the Rust library at launch (this also proves that
/// the Runner binary links the `hfa_discovery_*` symbols of the cargokit framework).
final class BonjourRegistrationTests: XCTestCase {
  func testBackendIsRegisteredAtLaunch() {
    XCTAssertTrue(HfaBonjourDiscovery.shared.isRegistered)
    XCTAssertTrue(HfaBonjourDiscovery.shared.register(), "registering again is harmless")
  }
}

final class PcmInterleaverTests: XCTestCase {
  /// Builds a linear PCM sample buffer holding `bytes` (the block buffer layout Core Media
  /// uses: interleaved frames, or one plane per channel for non-interleaved audio).
  private func makeSampleBuffer(
    rate: Double, channels: UInt32, bits: UInt32, flags: AudioFormatFlags, frames: Int,
    bytes: [UInt8]
  ) throws -> CMSampleBuffer {
    let nonInterleaved = flags & kAudioFormatFlagIsNonInterleaved != 0
    let bytesPerSample = bits / 8
    let bytesPerFrame = nonInterleaved ? bytesPerSample : bytesPerSample * channels
    var asbd = AudioStreamBasicDescription(
      mSampleRate: rate, mFormatID: kAudioFormatLinearPCM, mFormatFlags: flags,
      mBytesPerPacket: bytesPerFrame, mFramesPerPacket: 1, mBytesPerFrame: bytesPerFrame,
      mChannelsPerFrame: channels, mBitsPerChannel: bits, mReserved: 0)
    var format: CMAudioFormatDescription?
    let formatStatus = CMAudioFormatDescriptionCreate(
      allocator: kCFAllocatorDefault, asbd: &asbd, layoutSize: 0, layout: nil,
      magicCookieSize: 0, magicCookie: nil, extensions: nil, formatDescriptionOut: &format)
    XCTAssertEqual(formatStatus, noErr)
    let formatDescription = try XCTUnwrap(format)

    var block: CMBlockBuffer?
    let blockStatus = CMBlockBufferCreateWithMemoryBlock(
      allocator: kCFAllocatorDefault, memoryBlock: nil, blockLength: bytes.count,
      blockAllocator: kCFAllocatorDefault, customBlockSource: nil, offsetToData: 0,
      dataLength: bytes.count, flags: 0, blockBufferOut: &block)
    XCTAssertEqual(blockStatus, noErr)
    let blockBuffer = try XCTUnwrap(block)
    let copyStatus = bytes.withUnsafeBytes { raw -> OSStatus in
      guard let base = raw.baseAddress else { return -1 }
      return CMBlockBufferReplaceDataBytes(
        with: base, blockBuffer: blockBuffer, offsetIntoDestination: 0, dataLength: raw.count)
    }
    XCTAssertEqual(copyStatus, noErr)

    var sample: CMSampleBuffer?
    let sampleStatus = CMAudioSampleBufferCreateReadyWithPacketDescriptions(
      allocator: kCFAllocatorDefault, dataBuffer: blockBuffer, formatDescription: formatDescription,
      sampleCount: frames, presentationTimeStamp: .zero, packetDescriptions: nil,
      sampleBufferOut: &sample)
    XCTAssertEqual(sampleStatus, noErr)
    return try XCTUnwrap(sample)
  }

  private func samples(_ output: PcmInterleaver.Output) -> [Float] {
    Array(UnsafeBufferPointer(start: output.samples, count: output.frames * output.channels))
  }

  private func bigEndianBytes(_ values: [Int16]) -> [UInt8] {
    values.flatMap { v -> [UInt8] in
      let u = UInt16(bitPattern: v)
      return [UInt8(u >> 8), UInt8(u & 0xFF)]
    }
  }

  private func littleEndianBytes(_ values: [Float]) -> [UInt8] {
    values.flatMap { v -> [UInt8] in
      let u = v.bitPattern.littleEndian
      return withUnsafeBytes(of: u) { Array($0) }
    }
  }

  /// The usual ReplayKit `.audioApp` format: 44.1 kHz stereo big-endian Int16, interleaved.
  func testBigEndianInt16Interleaved() throws {
    let values: [Int16] = [0, 16_384, -16_384, 32_767, -32_768, 1]
    let buffer = try makeSampleBuffer(
      rate: 44_100, channels: 2, bits: 16,
      flags: kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsBigEndian
        | kAudioFormatFlagIsPacked,
      frames: 3, bytes: bigEndianBytes(values))
    let interleaver = PcmInterleaver()
    let output = try interleaver.convert(buffer).get()
    XCTAssertEqual(output.frames, 3)
    XCTAssertEqual(output.channels, 2)
    XCTAssertEqual(output.sampleRate, 44_100)
    let expected = values.map { Float($0) / 32_768 }
    XCTAssertEqual(samples(output), expected)
  }

  func testFloat32NonInterleavedBecomesInterleaved() throws {
    // Plane L = [0.1, 0.2], plane R = [-0.1, -0.2].
    let planes: [Float] = [0.1, 0.2, -0.1, -0.2]
    let buffer = try makeSampleBuffer(
      rate: 48_000, channels: 2, bits: 32,
      flags: kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked
        | kAudioFormatFlagIsNonInterleaved,
      frames: 2, bytes: littleEndianBytes(planes))
    let output = try PcmInterleaver().convert(buffer).get()
    XCTAssertEqual(output.frames, 2)
    XCTAssertEqual(samples(output), [0.1, -0.1, 0.2, -0.2])
  }

  func testLittleEndianInt16Mono() throws {
    let values: [Int16] = [8_192, -8_192]
    let bytes = values.flatMap { v -> [UInt8] in
      let u = UInt16(bitPattern: v)
      return [UInt8(u & 0xFF), UInt8(u >> 8)]
    }
    let buffer = try makeSampleBuffer(
      rate: 48_000, channels: 1, bits: 16,
      flags: kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsPacked, frames: 2, bytes: bytes)
    let output = try PcmInterleaver().convert(buffer).get()
    XCTAssertEqual(output.channels, 1)
    XCTAssertEqual(samples(output), [0.25, -0.25])
  }

  func testRejectsUnsignedEightBit() throws {
    let buffer = try makeSampleBuffer(
      rate: 48_000, channels: 1, bits: 8, flags: kAudioFormatFlagIsPacked, frames: 4,
      bytes: [128, 128, 128, 128])
    switch PcmInterleaver().convert(buffer) {
    case .success:
      XCTFail("8-bit unsigned PCM must be rejected")
    case let .failure(failure):
      guard case .unsupportedFormat = failure else {
        return XCTFail("unexpected failure \(failure)")
      }
    }
  }

  func testStorageIsReusedAcrossBuffers() throws {
    let interleaver = PcmInterleaver()
    let buffer = try makeSampleBuffer(
      rate: 44_100, channels: 2, bits: 16,
      flags: kAudioFormatFlagIsSignedInteger | kAudioFormatFlagIsBigEndian
        | kAudioFormatFlagIsPacked,
      frames: 2, bytes: bigEndianBytes([1, 2, 3, 4]))
    let first = try interleaver.convert(buffer).get().samples
    let second = try interleaver.convert(buffer).get().samples
    XCTAssertEqual(first, second, "no reallocation for same-sized buffers")
  }
}
