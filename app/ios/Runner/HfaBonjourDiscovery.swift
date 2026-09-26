// HfaBonjourDiscovery.swift - the iOS Bonjour backend of the Rust core's hub discovery
// (docs/CONTRACTS.md §8.9.2, core/hfa-ffi/include/hfa_discovery.h).
//
// The Rust core's own mDNS (mdns-sd) needs the restricted multicast entitlement on iOS, so the
// app registers this class with `hfa_discovery_register` at launch; `hfa_core::discovery` then
// browses and advertises through it, and everything above (the hub list, finding a hub by id,
// advertising an iPhone hub) works as on the other platforms.
//
// - Browsing: `NWBrowser` for `_hfa._tcp` in `local.` with TXT records (Apple's recommended
//   browse API; it also triggers the local network permission prompt). `NWBrowser` reports
//   service endpoints only, so every service is resolved with dns_sd: `DNSServiceResolve` (host
//   name, port, TXT) and then `DNSServiceGetAddrInfo` (IPv4 + IPv6 addresses). Both keep
//   running while the service is visible, so a changed port, TXT record or address is reported
//   again. This sends no packet to the hub itself (an `NWConnection` probe would open a TCP
//   connection to the hub's control port just to learn its address).
// - Advertising: `DNSServiceRegister` with the port the Rust hub already listens on. `NWListener`
//   cannot advertise a port it does not own, and `NetService` is deprecated.
// - Everything runs on one serial queue; the dns_sd references are scheduled on it
//   (`DNSServiceSetDispatchQueue`) and deallocated on it, so no callback runs after its object
//   was cancelled. Rust's callbacks only enqueue work and return at once.
// - Failures (mDNSResponder restarted, browser failed) are retried with a backoff of 1 s up to
//   30 s. A browse that Rust no longer listens to (`HFA_DISCOVERY_CLOSED`) stops itself.

import Foundation
import Network
import dnssd
import os

/// Registers the Bonjour backend with Rust and runs the browses and registrations it asks for.
final class HfaBonjourDiscovery: @unchecked Sendable {
  /// The process-wide instance (Rust's callbacks reach it without a context pointer).
  static let shared = HfaBonjourDiscovery()

  fileprivate static let log = Logger(
    subsystem: Bundle.main.bundleIdentifier ?? "headphone-for-all", category: "bonjour")

  /// Longest wait before a failed browse, resolve or registration is retried.
  fileprivate static let maxRetryDelay: TimeInterval = 30

  /// Serial queue of every browse, resolve and registration (all state below lives on it).
  fileprivate let queue = DispatchQueue(label: "hfa.bonjour")
  private var browses: [UInt64: BonjourBrowse] = [:]
  private var adverts: [UInt64: BonjourAdvert] = [:]

  private let registrationLock = NSLock()
  private var registered = false

  private init() {}

  /// Whether `register()` succeeded (Rust then uses this backend).
  var isRegistered: Bool {
    registrationLock.lock()
    defer { registrationLock.unlock() }
    return registered
  }

  /// Makes this class the Rust core's discovery backend. Call once at launch, before Flutter
  /// starts (so before any browse or hub start); later calls do nothing.
  @discardableResult
  func register() -> Bool {
    registrationLock.lock()
    defer { registrationLock.unlock() }
    if registered { return true }
    // C function pointers cannot capture context: they reach the singleton directly, and `ctx`
    // stays NULL.
    var callbacks = HfaDiscoveryCallbacks(
      ctx: nil,
      browse_start: { _, browseId in HfaBonjourDiscovery.shared.startBrowse(browseId) },
      browse_stop: { _, browseId in HfaBonjourDiscovery.shared.stopBrowse(browseId) },
      advertise_start: { _, advertId, json in
        HfaBonjourDiscovery.shared.startAdvert(advertId, json: json)
      },
      advertise_stop: { _, advertId in HfaBonjourDiscovery.shared.stopAdvert(advertId) })
    let status = hfa_discovery_register(&callbacks)
    guard status == HFA_DISCOVERY_OK else {
      Self.log.error(
        "cannot register the Bonjour backend (\(status)): \(Self.lastRustError(), privacy: .public)")
      return false
    }
    registered = true
    Self.log.info("Bonjour discovery backend registered")
    return true
  }

  /// Message of the last failed `hfa_discovery_*` call on this thread.
  fileprivate static func lastRustError() -> String {
    guard let message = hfa_discovery_last_error() else { return "unknown error" }
    return String(cString: message)
  }

  // MARK: - Rust callbacks (any thread; they only enqueue work)

  private func startBrowse(_ id: UInt64) -> Int32 {
    queue.async { [self] in
      let browse = BonjourBrowse(id: id, queue: queue) { [weak self] in
        self?.browses[id] = nil
      }
      browses[id] = browse
      browse.start()
    }
    return HFA_DISCOVERY_OK
  }

  private func stopBrowse(_ id: UInt64) {
    queue.async { [self] in
      browses.removeValue(forKey: id)?.stop()
    }
  }

  private func startAdvert(_ id: UInt64, json: UnsafePointer<CChar>?) -> Int32 {
    // The JSON is only valid during this call: decode it now.
    guard let json, let advert = HfaBonjourCodec.decodeAdvert(String(cString: json)) else {
      Self.log.error("invalid service registration from Rust")
      return HFA_DISCOVERY_ERR_INVALID_ARGUMENT
    }
    queue.async { [self] in
      let registration = BonjourAdvert(id: id, advert: advert, queue: queue)
      adverts[id] = registration
      registration.start()
    }
    return HFA_DISCOVERY_OK
  }

  private func stopAdvert(_ id: UInt64) {
    queue.async { [self] in
      adverts.removeValue(forKey: id)?.stop()
    }
  }
}

// MARK: - dns_sd helpers

/// Small helpers around the dns_sd C API.
private enum DNSSD {
  static let noError = DNSServiceErrorType(kDNSServiceErr_NoError)

  /// Schedules a freshly created reference on `queue`; `nil` (and a log line) if `error` says it
  /// was not created or scheduling fails.
  static func schedule(
    _ ref: DNSServiceRef?, _ error: DNSServiceErrorType, on queue: DispatchQueue, what: String
  ) -> DNSServiceRef? {
    guard error == noError, let ref else {
      HfaBonjourDiscovery.log.error("\(what, privacy: .public) failed: dns_sd error \(error)")
      return nil
    }
    let scheduled = DNSServiceSetDispatchQueue(ref, queue)
    guard scheduled == noError else {
      DNSServiceRefDeallocate(ref)
      HfaBonjourDiscovery.log.error(
        "\(what, privacy: .public): cannot schedule on the queue (dns_sd error \(scheduled))")
      return nil
    }
    return ref
  }

  /// The next retry delay after `delay` (doubling up to the maximum).
  static func backoff(_ delay: TimeInterval) -> TimeInterval {
    min(delay * 2, HfaBonjourDiscovery.maxRetryDelay)
  }
}

// MARK: - Browse

/// One browse session of Rust (`browse_id`). Queue-confined.
private final class BonjourBrowse: @unchecked Sendable {
  let id: UInt64
  private let queue: DispatchQueue
  private let onClosed: () -> Void
  private var browser: NWBrowser?
  /// Visible services by instance name.
  private var services: [String: BonjourService] = [:]
  private var stopped = false
  private var restartDelay: TimeInterval = 1

  init(id: UInt64, queue: DispatchQueue, onClosed: @escaping () -> Void) {
    self.id = id
    self.queue = queue
    self.onClosed = onClosed
  }

  func start() {
    guard !stopped, browser == nil else { return }
    let descriptor = NWBrowser.Descriptor.bonjourWithTXTRecord(
      type: HfaBonjourCodec.serviceType, domain: HfaBonjourCodec.domain)
    let browser = NWBrowser(for: descriptor, using: NWParameters())
    browser.stateUpdateHandler = { [weak self] state in self?.stateChanged(state) }
    browser.browseResultsChangedHandler = { [weak self] results, _ in self?.update(results) }
    self.browser = browser
    browser.start(queue: queue)
  }

  func stop() {
    stopped = true
    browser?.cancel()
    browser = nil
    for service in services.values {
      service.cancel()
    }
    services.removeAll()
  }

  private func stateChanged(_ state: NWBrowser.State) {
    switch state {
    case .ready:
      restartDelay = 1
      HfaBonjourDiscovery.log.info("browsing for hubs")
    case let .waiting(error):
      // Typically: local network access denied (Settings > Privacy > Local Network).
      HfaBonjourDiscovery.log.warning(
        "browsing waits: \(String(describing: error), privacy: .public)")
    case let .failed(error):
      HfaBonjourDiscovery.log.error(
        "browsing failed: \(String(describing: error), privacy: .public)")
      browser?.cancel()
      browser = nil
      let delay = restartDelay
      restartDelay = DNSSD.backoff(restartDelay)
      queue.asyncAfter(deadline: .now() + delay) { [weak self] in self?.start() }
    case .setup, .cancelled:
      break
    @unknown default:
      break
    }
  }

  /// Diffs the browser's full result set against the known services.
  private func update(_ results: Set<NWBrowser.Result>) {
    guard !stopped else { return }
    var current: [String: [String: String]] = [:]
    for result in results {
      guard case let .service(name, _, _, _) = result.endpoint else { continue }
      var txt: [String: String] = [:]
      if case let .bonjour(record) = result.metadata {
        txt = HfaBonjourCodec.normalizedTXT(record.dictionary)
      }
      // The same instance may be seen on several interfaces.
      current[name, default: [:]].merge(txt) { first, _ in first }
    }
    for name in Array(services.keys) where current[name] == nil {
      services.removeValue(forKey: name)?.cancel()
      let status = name.withCString { hfa_discovery_removed(id, $0) }
      if !handle(status) { return }
    }
    for (name, txt) in current {
      if let service = services[name] {
        service.browseTXT = txt
        report(service)
      } else {
        let service = BonjourService(name: name, browseTXT: txt, queue: queue) { [weak self] in
          self?.report($0)
        }
        services[name] = service
        service.start()
      }
      if stopped { return }
    }
  }

  /// Reports a service to Rust once it has a port and an address, and again when that changes.
  private func report(_ service: BonjourService) {
    guard !stopped, let json = service.reportJSON(), json != service.lastReported else { return }
    service.lastReported = json
    let status = json.withCString { hfa_discovery_resolved(id, $0) }
    _ = handle(status)
  }

  /// Handles a report's status; `false` when the browse stopped because Rust closed it.
  private func handle(_ status: Int32) -> Bool {
    if status == HFA_DISCOVERY_CLOSED {
      stop()
      onClosed()
      return false
    }
    if status != HFA_DISCOVERY_OK {
      HfaBonjourDiscovery.log.error(
        "Rust rejected a discovery report (\(status)): \(HfaBonjourDiscovery.lastRustError(), privacy: .public)"
      )
    }
    return true
  }
}

/// Resolves one visible service (host, port, TXT, addresses). Queue-confined.
private final class BonjourService {
  let name: String
  /// TXT record from the browser (preferred over the resolved one when not empty).
  var browseTXT: [String: String]
  /// The last JSON reported to Rust.
  var lastReported: String?
  private let queue: DispatchQueue
  private let onChange: (BonjourService) -> Void
  private var resolveRef: DNSServiceRef?
  private var addressRef: DNSServiceRef?
  private var host: String?
  private var port: UInt16 = 0
  private var resolvedTXT: [String: String] = [:]
  private var addresses = Set<String>()
  private var cancelled = false
  private var retryDelay: TimeInterval = 1

  init(
    name: String, browseTXT: [String: String], queue: DispatchQueue,
    onChange: @escaping (BonjourService) -> Void
  ) {
    self.name = name
    self.browseTXT = browseTXT
    self.queue = queue
    self.onChange = onChange
  }

  deinit {
    releaseReferences()
  }

  func start() {
    guard !cancelled, resolveRef == nil else { return }
    var ref: DNSServiceRef?
    let context = Unmanaged.passUnretained(self).toOpaque()
    let error = DNSServiceResolve(
      &ref, 0, 0, name, HfaBonjourCodec.serviceType, HfaBonjourCodec.domain,
      { _, _, _, errorCode, _, hostTarget, port, txtLength, txtRecord, context in
        // Runs on the queue; `context` is a live BonjourService (deallocation happens on the
        // same queue and stops the callbacks).
        guard let context else { return }
        let service = Unmanaged<BonjourService>.fromOpaque(context).takeUnretainedValue()
        let txt = txtRecord.map { Data(bytes: $0, count: Int(txtLength)) } ?? Data()
        service.resolved(
          errorCode: errorCode, host: hostTarget.map { String(cString: $0) },
          port: UInt16(bigEndian: port), txt: txt)
      }, context)
    guard let scheduled = DNSSD.schedule(ref, error, on: queue, what: "resolving \(name)") else {
      retryLater()
      return
    }
    resolveRef = scheduled
  }

  func cancel() {
    cancelled = true
    releaseReferences()
  }

  /// `{instance, txt, addrs, port}` for Rust, or `nil` while the port or addresses are unknown.
  func reportJSON() -> String? {
    guard port != 0, !addresses.isEmpty else { return nil }
    let txt = browseTXT.isEmpty ? resolvedTXT : browseTXT
    return HfaBonjourCodec.resolvedJSON(
      instance: name, txt: txt, addresses: addresses.sorted(), port: port)
  }

  private func resolved(errorCode: DNSServiceErrorType, host: String?, port: UInt16, txt: Data) {
    guard !cancelled else { return }
    guard errorCode == DNSSD.noError else {
      failed("resolving", errorCode)
      return
    }
    resolvedTXT = HfaBonjourCodec.parseTXTRecord(txt)
    self.port = port
    if let host, !host.isEmpty, host != self.host {
      self.host = host
      // On failure the resolution starts over later, with a growing delay.
      guard lookUpAddresses(of: host) else { return }
    }
    retryDelay = 1
    onChange(self)
  }

  /// Starts looking up the addresses of `host`; `false` when that could not start (the whole
  /// resolution is then released and retried later).
  private func lookUpAddresses(of host: String) -> Bool {
    if let addressRef {
      DNSServiceRefDeallocate(addressRef)
      self.addressRef = nil
    }
    addresses.removeAll()
    var ref: DNSServiceRef?
    let context = Unmanaged.passUnretained(self).toOpaque()
    let error = DNSServiceGetAddrInfo(
      &ref, 0, 0, DNSServiceProtocol(kDNSServiceProtocol_IPv4 | kDNSServiceProtocol_IPv6), host,
      { _, flags, _, errorCode, _, address, _, context in
        guard let context else { return }
        let service = Unmanaged<BonjourService>.fromOpaque(context).takeUnretainedValue()
        service.addressChanged(
          errorCode: errorCode, flags: flags, address: address.flatMap { HfaBonjourCodec.ipString($0) })
      }, context)
    guard let scheduled = DNSSD.schedule(ref, error, on: queue, what: "looking up \(host)") else {
      // Start the whole resolution over, as `failed` does: a retry of `start()` alone would
      // return at once while `resolveRef` lives, and later resolve callbacks with the same host
      // would never look its addresses up again, so the hub would never be reported.
      releaseReferences()
      retryLater()
      return false
    }
    addressRef = scheduled
    return true
  }

  private func addressChanged(errorCode: DNSServiceErrorType, flags: DNSServiceFlags, address: String?) {
    guard !cancelled else { return }
    guard errorCode == DNSSD.noError else {
      failed("looking up \(host ?? name)", errorCode)
      return
    }
    if let address {
      if flags & kDNSServiceFlagsAdd != 0 {
        addresses.insert(address)
      } else {
        addresses.remove(address)
      }
    }
    if flags & kDNSServiceFlagsMoreComing == 0 {
      onChange(self)
    }
  }

  /// An asynchronous dns_sd error (e.g. mDNSResponder restarted): start over later.
  private func failed(_ what: String, _ errorCode: DNSServiceErrorType) {
    HfaBonjourDiscovery.log.warning(
      "\(what, privacy: .public) \(self.name, privacy: .public) failed: dns_sd error \(errorCode)")
    releaseReferences()
    retryLater()
  }

  private func retryLater() {
    guard !cancelled else { return }
    let delay = retryDelay
    retryDelay = DNSSD.backoff(retryDelay)
    queue.asyncAfter(deadline: .now() + delay) { [weak self] in self?.start() }
  }

  private func releaseReferences() {
    if let resolveRef {
      DNSServiceRefDeallocate(resolveRef)
      self.resolveRef = nil
    }
    if let addressRef {
      DNSServiceRefDeallocate(addressRef)
      self.addressRef = nil
    }
    host = nil
  }
}

// MARK: - Advertise

/// One hub registration of Rust (`advert_id`). Queue-confined.
private final class BonjourAdvert {
  let id: UInt64
  private let advert: HfaBonjourCodec.Advert
  private let queue: DispatchQueue
  private var ref: DNSServiceRef?
  private var stopped = false
  private var retryDelay: TimeInterval = 1

  init(id: UInt64, advert: HfaBonjourCodec.Advert, queue: DispatchQueue) {
    self.id = id
    self.advert = advert
    self.queue = queue
  }

  deinit {
    if let ref {
      DNSServiceRefDeallocate(ref)
    }
  }

  func start() {
    guard !stopped, ref == nil else { return }
    let txt = HfaBonjourCodec.txtRecordData(advert.txt)
    guard let txtLength = UInt16(exactly: txt.count) else {
      HfaBonjourDiscovery.log.error("TXT record too long (\(txt.count) bytes)")
      return
    }
    var newRef: DNSServiceRef?
    let context = Unmanaged.passUnretained(self).toOpaque()
    let error = txt.withUnsafeBytes { raw -> DNSServiceErrorType in
      DNSServiceRegister(
        &newRef, 0, 0, advert.instance, advert.type, advert.domain, nil, advert.port.bigEndian,
        txtLength, raw.baseAddress,
        { _, _, errorCode, name, _, _, context in
          guard let context else { return }
          let registration = Unmanaged<BonjourAdvert>.fromOpaque(context).takeUnretainedValue()
          registration.registered(errorCode: errorCode, name: name.map { String(cString: $0) })
        }, context)
    }
    guard let scheduled = DNSSD.schedule(newRef, error, on: queue, what: "advertising the hub") else {
      retryLater()
      return
    }
    ref = scheduled
  }

  func stop() {
    stopped = true
    if let ref {
      DNSServiceRefDeallocate(ref)
      self.ref = nil
    }
  }

  private func registered(errorCode: DNSServiceErrorType, name: String?) {
    guard !stopped else { return }
    guard errorCode == DNSSD.noError else {
      HfaBonjourDiscovery.log.warning("advertising failed: dns_sd error \(errorCode); retrying")
      if let ref {
        DNSServiceRefDeallocate(ref)
        self.ref = nil
      }
      retryLater()
      return
    }
    retryDelay = 1
    HfaBonjourDiscovery.log.info(
      "advertising the hub as \(name ?? self.advert.instance, privacy: .public) on port \(self.advert.port)")
  }

  private func retryLater() {
    guard !stopped else { return }
    let delay = retryDelay
    retryDelay = DNSSD.backoff(retryDelay)
    queue.asyncAfter(deadline: .now() + delay) { [weak self] in self?.start() }
  }
}
