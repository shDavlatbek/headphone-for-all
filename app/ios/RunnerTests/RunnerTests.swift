// RunnerTests.swift - unit tests of the iOS native code that can run without a device:
// the App Group file formats (HfaShared.swift, compiled into Runner) and the ReplayKit PCM
// conversion of the broadcast extension (PcmInterleaver.swift is compiled into this test target
// too, since an app extension cannot be a test host).
//
// Run on a Mac: xcodebuild test -workspace ios/Runner.xcworkspace -scheme Runner \
//   -destination 'platform=iOS Simulator,name=iPhone 16'   (after `flutter build ios --simulator`)

import AudioToolbox
import CoreMedia
import XCTest

@testable import Runner

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

  func testStatusRoundTrips() throws {
    let status = BroadcastStatus(state: "finished", message: "hub not paired", timestamp: 12.5)
    let data = try JSONEncoder().encode(status)
    XCTAssertEqual(try JSONDecoder().decode(BroadcastStatus.self, from: data), status)
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
