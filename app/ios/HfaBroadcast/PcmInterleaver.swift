// PcmInterleaver.swift - ReplayKit audio sample buffers -> interleaved Float32.
//
// ReplayKit reveals the audio format per buffer. `.audioApp` buffers are usually 44.1 kHz stereo
// signed 16-bit, often big-endian; other devices / iOS versions deliver 48 kHz or Float32, and
// the layout may be interleaved (one AudioBuffer) or non-interleaved (one AudioBuffer per
// channel). This converter accepts linear PCM Int16 / Int32 / Float32 / Float64 in either byte
// order and either layout, and writes interleaved Float32 in [-1, 1] into storage that is
// reused across calls (the extension runs under a ~50 MB memory cap, so nothing is allocated
// per buffer once the storage has grown to the largest buffer seen). Rate and channel count
// are passed on unchanged: Rust (`hfa_ext_push_pcm`) resamples to 48 kHz stereo.

import AudioToolbox
import CoreMedia
import Foundation

/// Converts `CMSampleBuffer` linear PCM to interleaved `Float` samples.
final class PcmInterleaver {
  /// Why a buffer could not be converted (reported once per kind by the caller).
  enum Failure: Error, Equatable, CustomStringConvertible {
    case noFormatDescription
    case unsupportedFormat(formatID: UInt32, flags: UInt32, bits: UInt32)
    case unsupportedChannels(UInt32)
    case unsupportedRate(Double)
    case bufferList(OSStatus)
    case layoutMismatch(buffers: Int, channels: UInt32)
    case tooLarge(samples: Int)

    var description: String {
      switch self {
      case .noFormatDescription:
        return "sample buffer without an audio format description"
      case let .unsupportedFormat(formatID, flags, bits):
        return "unsupported audio format: id \(formatID), flags 0x\(String(flags, radix: 16)), \(bits) bits"
      case let .unsupportedChannels(n):
        return "unsupported channel count: \(n)"
      case let .unsupportedRate(r):
        return "unsupported sample rate: \(r) Hz"
      case let .bufferList(status):
        return "CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer failed: \(status)"
      case let .layoutMismatch(buffers, channels):
        return "\(buffers) audio buffers for \(channels) channels"
      case let .tooLarge(samples):
        return "sample buffer too large: \(samples) samples"
      }
    }
  }

  /// One converted buffer. `samples` points into the converter's storage and is valid until
  /// the next `convert` call.
  struct Output {
    let samples: UnsafePointer<Float>
    let frames: Int
    let channels: Int
    let sampleRate: Int
  }

  /// Sample encodings the converter understands.
  private enum Encoding {
    case int16, int32, float32, float64

    var bytes: Int {
      switch self {
      case .int16: return 2
      case .int32, .float32: return 4
      case .float64: return 8
      }
    }
  }

  /// Largest buffer accepted (the Rust side's per-call limit).
  static let maxSamples = 1_536_000
  /// Channel counts accepted by the Rust side.
  static let channelRange: ClosedRange<UInt32> = 1...8
  /// Sample rates accepted by the Rust side.
  static let rateRange: ClosedRange<Double> = 8_000...192_000

  private var storage: UnsafeMutablePointer<Float>
  private var capacity: Int
  /// Reused memory for the AudioBufferList (its size depends on the channel layout).
  private var listStorage: UnsafeMutableRawPointer
  private var listCapacity: Int

  init() {
    // 4096 stereo frames covers the usual ~1024-frame ReplayKit buffers without regrowth.
    capacity = 8_192
    storage = UnsafeMutablePointer<Float>.allocate(capacity: capacity)
    listCapacity = MemoryLayout<AudioBufferList>.size + 8 * MemoryLayout<AudioBuffer>.size
    listStorage = UnsafeMutableRawPointer.allocate(
      byteCount: listCapacity, alignment: MemoryLayout<AudioBufferList>.alignment)
  }

  deinit {
    storage.deallocate()
    listStorage.deallocate()
  }

  /// Converts one ReplayKit audio sample buffer.
  func convert(_ sampleBuffer: CMSampleBuffer) -> Result<Output, Failure> {
    guard let format = CMSampleBufferGetFormatDescription(sampleBuffer),
      let asbdPointer = CMAudioFormatDescriptionGetStreamBasicDescription(format)
    else { return .failure(.noFormatDescription) }
    let asbd = asbdPointer.pointee
    guard let encoding = Self.encoding(of: asbd) else {
      return .failure(
        .unsupportedFormat(
          formatID: asbd.mFormatID, flags: asbd.mFormatFlags, bits: asbd.mBitsPerChannel))
    }
    let channels = asbd.mChannelsPerFrame
    guard Self.channelRange.contains(channels) else {
      return .failure(.unsupportedChannels(channels))
    }
    guard Self.rateRange.contains(asbd.mSampleRate) else {
      return .failure(.unsupportedRate(asbd.mSampleRate))
    }
    let frameCount = CMSampleBufferGetNumSamples(sampleBuffer)
    let channelCount = Int(channels)
    guard frameCount > 0 else {
      return .success(
        Output(
          samples: UnsafePointer(storage), frames: 0, channels: channelCount,
          sampleRate: Int(asbd.mSampleRate.rounded())))
    }
    let wanted = frameCount * channelCount
    guard wanted <= Self.maxSamples else { return .failure(.tooLarge(samples: wanted)) }
    reserve(samples: wanted)

    // First ask for the size of the AudioBufferList (non-interleaved audio needs one
    // AudioBuffer per channel), then fetch it into reused memory.
    let flags = UInt32(kCMSampleBufferFlag_AudioBufferList_Assure16ByteAlignment)
    var listSize = 0
    var status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
      sampleBuffer,
      bufferListSizeNeededOut: &listSize,
      bufferListOut: nil,
      bufferListSize: 0,
      blockBufferAllocator: nil,
      blockBufferMemoryAllocator: nil,
      flags: flags,
      blockBufferOut: nil)
    guard status == noErr, listSize > 0 else { return .failure(.bufferList(status)) }
    reserveList(bytes: listSize)
    let list = listStorage.bindMemory(to: AudioBufferList.self, capacity: 1)
    var blockBuffer: CMBlockBuffer?
    status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
      sampleBuffer,
      bufferListSizeNeededOut: nil,
      bufferListOut: list,
      bufferListSize: listSize,
      blockBufferAllocator: nil,
      blockBufferMemoryAllocator: nil,
      flags: flags,
      blockBufferOut: &blockBuffer)
    guard status == noErr, blockBuffer != nil else { return .failure(.bufferList(status)) }
    // `blockBuffer` retains the sample memory the list points to until the end of this scope.
    let frames: Int
    let result: Result<Int, Failure> = withExtendedLifetime(blockBuffer) {
      self.interleave(
        list: UnsafeMutableAudioBufferListPointer(list), asbd: asbd, encoding: encoding,
        frames: frameCount)
    }
    switch result {
    case let .success(n): frames = n
    case let .failure(f): return .failure(f)
    }
    return .success(
      Output(
        samples: UnsafePointer(storage), frames: frames, channels: channelCount,
        sampleRate: Int(asbd.mSampleRate.rounded())))
  }

  /// Linear PCM encodings accepted by `convert`, or `nil`.
  private static func encoding(of asbd: AudioStreamBasicDescription) -> Encoding? {
    guard asbd.mFormatID == kAudioFormatLinearPCM else { return nil }
    let isFloat = asbd.mFormatFlags & kAudioFormatFlagIsFloat != 0
    switch (isFloat, asbd.mBitsPerChannel) {
    case (true, 32): return .float32
    case (true, 64): return .float64
    case (false, 16):
      return asbd.mFormatFlags & kAudioFormatFlagIsSignedInteger != 0 ? .int16 : nil
    case (false, 32):
      return asbd.mFormatFlags & kAudioFormatFlagIsSignedInteger != 0 ? .int32 : nil
    default: return nil
    }
  }

  /// Copies the buffers into `storage` as interleaved Float. Returns the frames written.
  private func interleave(
    list: UnsafeMutableAudioBufferListPointer,
    asbd: AudioStreamBasicDescription,
    encoding: Encoding,
    frames frameCount: Int
  ) -> Result<Int, Failure> {
    let channels = Int(asbd.mChannelsPerFrame)
    let bigEndian = asbd.mFormatFlags & kAudioFormatFlagIsBigEndian != 0
    let nonInterleaved = asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0
    if nonInterleaved {
      // One AudioBuffer per channel; mBytesPerFrame describes one channel.
      guard list.count == channels else {
        return .failure(.layoutMismatch(buffers: list.count, channels: asbd.mChannelsPerFrame))
      }
      let stride = asbd.mBytesPerFrame > 0 ? Int(asbd.mBytesPerFrame) : encoding.bytes
      var frames = frameCount
      for buffer in list {
        frames = min(frames, Int(buffer.mDataByteSize) / stride)
      }
      for (channel, buffer) in list.enumerated() {
        guard let data = buffer.mData else { return .success(0) }
        for frame in 0..<frames {
          storage[frame * channels + channel] = Self.sample(
            UnsafeRawPointer(data), offset: frame * stride, encoding: encoding,
            bigEndian: bigEndian)
        }
      }
      return .success(frames)
    }
    // Interleaved: normally one AudioBuffer holding every channel.
    guard list.count == 1, let data = list[0].mData else {
      return .failure(.layoutMismatch(buffers: list.count, channels: asbd.mChannelsPerFrame))
    }
    let stride = asbd.mBytesPerFrame > 0 ? Int(asbd.mBytesPerFrame) : encoding.bytes * channels
    guard stride >= encoding.bytes * channels else {
      return .failure(
        .unsupportedFormat(
          formatID: asbd.mFormatID, flags: asbd.mFormatFlags, bits: asbd.mBitsPerChannel))
    }
    let frames = min(frameCount, Int(list[0].mDataByteSize) / stride)
    let raw = UnsafeRawPointer(data)
    var out = 0
    for frame in 0..<frames {
      let base = frame * stride
      for channel in 0..<channels {
        storage[out] = Self.sample(
          raw, offset: base + channel * encoding.bytes, encoding: encoding, bigEndian: bigEndian)
        out += 1
      }
    }
    return .success(frames)
  }

  /// Reads one sample at `offset` bytes and scales it to Float in [-1, 1].
  @inline(__always)
  private static func sample(
    _ raw: UnsafeRawPointer, offset: Int, encoding: Encoding, bigEndian: Bool
  ) -> Float {
    switch encoding {
    case .int16:
      let bits = raw.loadUnaligned(fromByteOffset: offset, as: Int16.self)
      return Float(bigEndian ? Int16(bigEndian: bits) : Int16(littleEndian: bits)) / 32_768
    case .int32:
      let bits = raw.loadUnaligned(fromByteOffset: offset, as: Int32.self)
      return Float(bigEndian ? Int32(bigEndian: bits) : Int32(littleEndian: bits))
        / 2_147_483_648
    case .float32:
      let bits = raw.loadUnaligned(fromByteOffset: offset, as: UInt32.self)
      return Float(bitPattern: bigEndian ? UInt32(bigEndian: bits) : UInt32(littleEndian: bits))
    case .float64:
      let bits = raw.loadUnaligned(fromByteOffset: offset, as: UInt64.self)
      return Float(
        Double(bitPattern: bigEndian ? UInt64(bigEndian: bits) : UInt64(littleEndian: bits)))
    }
  }

  /// Grows the sample storage to at least `samples` floats (rarely: buffers have a stable size).
  private func reserve(samples: Int) {
    guard samples > capacity else { return }
    storage.deallocate()
    capacity = samples
    storage = UnsafeMutablePointer<Float>.allocate(capacity: capacity)
  }

  /// Grows the AudioBufferList memory to at least `bytes`.
  private func reserveList(bytes: Int) {
    guard bytes > listCapacity else { return }
    listStorage.deallocate()
    listCapacity = bytes
    listStorage = UnsafeMutableRawPointer.allocate(
      byteCount: listCapacity, alignment: MemoryLayout<AudioBufferList>.alignment)
  }
}
