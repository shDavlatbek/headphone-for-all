// HfaBonjourCodec.swift - the pure (I/O-free) parts of the iOS Bonjour discovery backend
// (HfaBonjourDiscovery.swift): DNS-SD TXT records, socket addresses and the JSON documents
// exchanged with the Rust core through core/hfa-ffi/include/hfa_discovery.h
// (docs/CONTRACTS.md §8.9.2). Unit-tested in RunnerTests.

import Foundation

/// Encoders and decoders of the Bonjour discovery backend.
enum HfaBonjourCodec {
  /// Bonjour service type of a hub (`hfa_proto::SERVICE_TYPE` without the domain). Must be
  /// listed in Info.plist `NSBonjourServices`.
  static let serviceType = "_hfa._tcp"
  /// Bonjour domain of `serviceType`.
  static let domain = "local."
  /// Longest `key=value` string of a TXT record (one length byte).
  static let maxTXTEntryLength = 255

  /// One TXT record entry.
  struct TXTEntry: Equatable {
    /// The key (`v`, `id`, `name`, `platform`).
    let key: String
    /// The value.
    let value: String
  }

  /// A service registration requested by Rust (`advertise_start`'s JSON).
  struct Advert: Equatable {
    /// Instance name (`Desk (ab12)`).
    let instance: String
    /// Service type (`_hfa._tcp`).
    let type: String
    /// Domain (`local.`).
    let domain: String
    /// TCP port the Rust hub listens on (host byte order, never 0).
    let port: UInt16
    /// TXT record entries in order.
    let txt: [TXTEntry]
  }

  /// Wire form of `Advert`.
  private struct AdvertJSON: Decodable {
    let instance: String
    let type: String
    let domain: String
    let port: Int
    let txt: [[String]]
  }

  // MARK: - TXT records

  /// DNS-SD TXT record bytes (RFC 6763 §6): every entry as a length-prefixed `key=value`
  /// string. Entries with an empty key, a key containing `=`, or longer than 255 bytes are left
  /// out; an empty record is the single empty string (one zero byte), as RFC 6763 requires.
  static func txtRecordData(_ entries: [TXTEntry]) -> Data {
    var data = Data()
    for entry in entries {
      guard !entry.key.isEmpty, !entry.key.contains("=") else { continue }
      let bytes = Array("\(entry.key)=\(entry.value)".utf8)
      guard bytes.count <= maxTXTEntryLength else { continue }
      data.append(UInt8(bytes.count))
      data.append(contentsOf: bytes)
    }
    if data.isEmpty {
      data.append(0)
    }
    return data
  }

  /// Parses TXT record bytes into a dictionary with lower-cased keys (DNS-SD keys are
  /// case-insensitive). The first occurrence of a key wins (RFC 6763 §6.4); an entry without
  /// `=` has an empty value; entries that are not UTF-8 and a truncated tail are ignored.
  static func parseTXTRecord(_ data: Data) -> [String: String] {
    let bytes = [UInt8](data)
    var result: [String: String] = [:]
    var index = 0
    while index < bytes.count {
      let length = Int(bytes[index])
      index += 1
      guard index + length <= bytes.count else { break }
      let entry = bytes[index..<(index + length)]
      index += length
      let separator = entry.firstIndex(of: UInt8(ascii: "="))
      let keyBytes = entry[entry.startIndex..<(separator ?? entry.endIndex)]
      let valueBytes = separator.map { entry[($0 + 1)..<entry.endIndex] } ?? []
      guard !keyBytes.isEmpty,
        let key = String(bytes: keyBytes, encoding: .utf8),
        let value = String(bytes: valueBytes, encoding: .utf8)
      else { continue }
      let normalized = key.lowercased()
      if result[normalized] == nil {
        result[normalized] = value
      }
    }
    return result
  }

  /// Lower-cases the keys of a TXT dictionary (for example `NWTXTRecord.dictionary`); on a
  /// collision the value of the smallest original key wins, so the result is deterministic.
  static func normalizedTXT(_ txt: [String: String]) -> [String: String] {
    var result: [String: String] = [:]
    for key in txt.keys.sorted() {
      let normalized = key.lowercased()
      if result[normalized] == nil, let value = txt[key] {
        result[normalized] = value
      }
    }
    return result
  }

  // MARK: - Addresses

  /// The numeric text of an IPv4 or IPv6 socket address, or `nil` for other families and for
  /// IPv6 link-local addresses (unusable without their zone, which the engine does not keep).
  static func ipString(_ address: UnsafePointer<sockaddr>) -> String? {
    switch Int32(address.pointee.sa_family) {
    case AF_INET:
      return address.withMemoryRebound(to: sockaddr_in.self, capacity: 1) { pointer in
        var raw = pointer.pointee.sin_addr
        return withUnsafeBytes(of: &raw) { numericHost(family: AF_INET, bytes: $0) }
      }
    case AF_INET6:
      return address.withMemoryRebound(to: sockaddr_in6.self, capacity: 1) { pointer in
        var raw = pointer.pointee.sin6_addr
        return withUnsafeBytes(of: &raw) { bytes -> String? in
          guard bytes.count == 16 else { return nil }
          // fe80::/10
          if bytes[0] == 0xFE, bytes[1] & 0xC0 == 0x80 { return nil }
          return numericHost(family: AF_INET6, bytes: bytes)
        }
      }
    default:
      return nil
    }
  }

  /// `inet_ntop` of an `in_addr` / `in6_addr`.
  private static func numericHost(family: Int32, bytes: UnsafeRawBufferPointer) -> String? {
    guard let source = bytes.baseAddress else { return nil }
    var buffer = [CChar](repeating: 0, count: Int(INET6_ADDRSTRLEN) + 1)
    return buffer.withUnsafeMutableBufferPointer { output -> String? in
      guard let destination = output.baseAddress,
        inet_ntop(family, source, destination, socklen_t(output.count)) != nil
      else { return nil }
      return String(cString: destination)
    }
  }

  // MARK: - JSON with Rust

  /// The `hfa_discovery_resolved` document: `{instance, txt, addrs, port}` (sorted keys).
  static func resolvedJSON(instance: String, txt: [String: String], addresses: [String], port: UInt16)
    -> String?
  {
    let object: [String: Any] = [
      "instance": instance, "txt": txt, "addrs": addresses, "port": Int(port),
    ]
    guard JSONSerialization.isValidJSONObject(object),
      let data = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys])
    else { return nil }
    return String(data: data, encoding: .utf8)
  }

  /// Decodes `advertise_start`'s JSON; `nil` when it is malformed, the port is outside
  /// 1...65535, a name is empty or a TXT entry is not a `[key, value]` pair.
  static func decodeAdvert(_ json: String) -> Advert? {
    guard let wire = try? JSONDecoder().decode(AdvertJSON.self, from: Data(json.utf8)),
      !wire.instance.isEmpty, !wire.type.isEmpty, !wire.domain.isEmpty,
      let port = UInt16(exactly: wire.port), port != 0
    else { return nil }
    var entries: [TXTEntry] = []
    for pair in wire.txt {
      guard pair.count == 2 else { return nil }
      entries.append(TXTEntry(key: pair[0], value: pair[1]))
    }
    return Advert(
      instance: wire.instance, type: wire.type, domain: wire.domain, port: port, txt: entries)
  }
}
