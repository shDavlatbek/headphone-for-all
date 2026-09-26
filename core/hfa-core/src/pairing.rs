//! Hub-side pairing windows.
//!
//! [`PairingManager::start`] opens a window with a fresh 6-digit PIN and one-time token. While
//! it is open, an unknown sender may pair using either secret (SPAKE2, see
//! [`hfa_proto::pairing`]).
//!
//! Online guessing limit: every SPAKE2 run gives the peer one guess, so the hub obtains the
//! secret only through [`PairingManager::begin_attempt`], which **counts the attempt before
//! SPAKE2 runs** and allows **one attempt in flight per window**. Parallel connections therefore
//! cannot get more than [`MAX_FAILED_ATTEMPTS`] guesses per window: once that many attempts
//! have been handed out and none succeeded, the window is closed.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use hfa_proto::control::PairMethod;
use serde::{Deserialize, Serialize};

/// Pairing attempts (SPAKE2 runs) allowed per window; after this many unsuccessful attempts
/// the window is closed.
pub const MAX_FAILED_ATTEMPTS: u32 = 5;
/// Default lifetime of a pairing window.
pub const DEFAULT_PAIRING_TTL: Duration = Duration::from_secs(300);

/// What the hub shows the user while pairing is open. `Debug` redacts the PIN, the token and
/// the URI (which contains the token).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingInfo {
    /// 6-digit PIN.
    pub pin: String,
    /// One-time token (also inside `uri`).
    pub token: String,
    /// `hfa://pair?...` URI for the QR code.
    pub uri: String,
    /// Unix time (seconds) when the window closes.
    pub expires_at_unix: u64,
}

impl fmt::Debug for PairingInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairingInfo")
            .field("pin", &"<redacted>")
            .field("token", &"<redacted>")
            .field("uri", &"<redacted>")
            .field("expires_at_unix", &self.expires_at_unix)
            .finish()
    }
}

/// An open pairing window.
struct Window {
    /// Distinguishes windows, so a stale [`PairingAttempt`] never touches a newer window.
    generation: u64,
    info: PairingInfo,
    /// Attempts handed out by `begin_attempt` (counted up front).
    attempts: u32,
    /// `true` while a [`PairingAttempt`] of this window is alive.
    in_flight: bool,
}

#[derive(Default)]
struct State {
    window: Option<Window>,
    next_generation: u64,
}

/// Source of the current unix time in seconds (injectable for tests, see
/// [`PairingManager::with_clock`]).
pub type UnixClock = Arc<dyn Fn() -> u64 + Send + Sync>;

/// Hub-side pairing state. `Send + Sync`; share it with `Arc`. `Debug` never prints secrets.
pub struct PairingManager {
    hub_id: [u8; 32],
    name: String,
    port: u16,
    clock: UnixClock,
    state: parking_lot::Mutex<State>,
}

impl fmt::Debug for PairingManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.state.lock();
        f.debug_struct("PairingManager")
            .field("hub_id", &self.hub_id)
            .field("name", &self.name)
            .field("port", &self.port)
            .field("window_open", &state.window.is_some())
            .field("attempts", &state.window.as_ref().map(|w| w.attempts))
            .finish()
    }
}

impl PairingManager {
    /// Creates a manager for the hub with static public key `hub_id`, display `name`, listening
    /// on `port` (used to build the URI; the host is the hub's primary LAN address).
    pub fn new(hub_id: [u8; 32], name: String, port: u16) -> Self {
        Self::with_clock(hub_id, name, port, Arc::new(crate::identity::unix_now))
    }

    /// Like [`PairingManager::new`], with a custom clock returning unix seconds (tests use it
    /// to expire windows without waiting).
    pub fn with_clock(hub_id: [u8; 32], name: String, port: u16, clock: UnixClock) -> Self {
        Self {
            hub_id,
            name,
            port,
            clock,
            state: parking_lot::Mutex::new(State::default()),
        }
    }

    /// Opens (or replaces) a pairing window valid for `ttl` (rounded up to whole seconds),
    /// with a fresh PIN and token and a zero attempt counter. A replaced window's in-flight
    /// attempt can no longer succeed ([`PairingAttempt::succeed`] returns `false`) or touch
    /// the new window.
    ///
    /// The URI's host is the hub's best LAN IPv4 address (see [`lan_ipv4`]), or `127.0.0.1`
    /// if there is none. If the URI cannot be built (the manager was created with port 0),
    /// `uri` is empty and a warning is logged; the PIN still works.
    pub fn start(&self, ttl: Duration) -> PairingInfo {
        let pin = hfa_proto::generate_pin();
        let token = hfa_proto::generate_token();
        let host = lan_ipv4().unwrap_or(Ipv4Addr::LOCALHOST).to_string();
        let uri = match hfa_proto::PairingUri::new(
            &host,
            self.port,
            self.hub_id,
            token.clone(),
            &self.name,
        ) {
            Ok(uri) => uri.to_string(),
            Err(e) => {
                tracing::warn!(error = %e, port = self.port, "cannot build the pairing URI");
                String::new()
            }
        };
        let secs = ttl.as_secs() + u64::from(ttl.subsec_nanos() > 0);
        let info = PairingInfo {
            pin,
            token,
            uri,
            expires_at_unix: (self.clock)().saturating_add(secs),
        };
        self.open_window(info.clone());
        tracing::info!(
            expires_at_unix = info.expires_at_unix,
            "pairing window opened"
        );
        info
    }

    /// Closes the current window. An attempt in flight can no longer succeed
    /// ([`PairingAttempt::succeed`] returns `false`).
    pub fn cancel(&self) {
        self.state.lock().window = None;
    }

    /// The open window, if any and not expired.
    pub fn current(&self) -> Option<PairingInfo> {
        let now = (self.clock)();
        let mut state = self.state.lock();
        close_if_expired(&mut state, now);
        state.window.as_ref().map(|w| w.info.clone())
    }

    /// Starts one pairing attempt with `method` and returns the secret (PIN or token) to run
    /// SPAKE2 with, wrapped in a guard.
    ///
    /// Atomically (under the window lock) it checks that a window is open and unexpired, that
    /// no other attempt is in flight and that fewer than [`MAX_FAILED_ATTEMPTS`] attempts were
    /// made, then **counts the attempt**. Returns `None` otherwise (and for
    /// `PairMethod::Unspecified`, which does not count). Call [`PairingAttempt::succeed`] when
    /// the peer's confirmation MAC verified (and trust the peer only if it returns `true`);
    /// dropping the guard in any other way (wrong secret, error, disconnect, timeout) is a
    /// failed attempt, and the window closes once the limit is reached.
    pub fn begin_attempt(&self, method: PairMethod) -> Option<PairingAttempt<'_>> {
        let now = (self.clock)();
        let mut state = self.state.lock();
        close_if_expired(&mut state, now);
        let window = state.window.as_mut()?;
        if window.in_flight || window.attempts >= MAX_FAILED_ATTEMPTS {
            return None;
        }
        let secret = match method {
            PairMethod::Pin => window.info.pin.clone(),
            PairMethod::Token => window.info.token.clone(),
            PairMethod::Unspecified => return None,
        };
        window.attempts += 1;
        window.in_flight = true;
        Some(PairingAttempt {
            manager: self,
            generation: window.generation,
            secret,
            finished: false,
        })
    }

    /// Installs `info` as the open window (fresh counter, new generation). `start` builds the
    /// PIN, token and URI and then calls this.
    fn open_window(&self, info: PairingInfo) {
        let mut state = self.state.lock();
        let generation = state.next_generation;
        state.next_generation += 1;
        state.window = Some(Window {
            generation,
            info,
            attempts: 0,
            in_flight: false,
        });
    }

    /// Ends the attempt of window `generation` as a failure (no-op for a stale generation).
    fn fail_attempt(&self, generation: u64) {
        let mut state = self.state.lock();
        let Some(window) = state.window.as_mut() else {
            return;
        };
        if window.generation != generation {
            // The window was cancelled or replaced meanwhile; leave the new one alone.
            return;
        }
        window.in_flight = false;
        if window.attempts >= MAX_FAILED_ATTEMPTS {
            // Guess budget used up.
            state.window = None;
        }
    }

    /// Commits a successful attempt of window `generation`: `true` and the window closes if
    /// it is still the current, unexpired window; `false` (nothing changes) otherwise.
    fn commit_attempt(&self, generation: u64) -> bool {
        let now = (self.clock)();
        let mut state = self.state.lock();
        close_if_expired(&mut state, now);
        match &state.window {
            Some(window) if window.generation == generation => {
                // PIN and token are one-time.
                state.window = None;
                true
            }
            _ => false,
        }
    }
}

/// One in-flight pairing attempt (see [`PairingManager::begin_attempt`]). Dropping it
/// without a successful [`PairingAttempt::succeed`] records a failure.
pub struct PairingAttempt<'a> {
    manager: &'a PairingManager,
    generation: u64,
    secret: String,
    /// Set once `succeed` ran (whatever its outcome), so `Drop` does nothing more.
    finished: bool,
}

impl PairingAttempt<'_> {
    /// The PIN or token to run SPAKE2 with.
    pub fn secret(&self) -> &str {
        &self.secret
    }

    /// The peer proved knowledge of the secret: atomically (under the window lock) checks
    /// that this attempt's window is still open — not cancelled, replaced by a new
    /// [`PairingManager::start`] or expired — and if so closes it (one-time secrets) and
    /// returns `true`. Returns `false` otherwise: the pairing must then be refused (the user
    /// withdrew the secret while the attempt was running), and nothing else changes.
    #[must_use = "a `false` result means the pairing was cancelled and must be refused"]
    pub fn succeed(mut self) -> bool {
        self.finished = true;
        self.manager.commit_attempt(self.generation)
    }
}

impl Drop for PairingAttempt<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.manager.fail_attempt(self.generation);
        }
    }
}

impl fmt::Debug for PairingAttempt<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairingAttempt")
            .field("generation", &self.generation)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// The best IPv4 address of this device for other LAN devices to reach it: an up,
/// non-loopback, non-link-local, non-point-to-point interface, preferring private
/// (RFC 1918) addresses and physical-looking interfaces over virtual ones (Docker, VM,
/// VPN bridges). `None` if there is none.
pub fn lan_ipv4() -> Option<Ipv4Addr> {
    lan_candidates()
        .into_iter()
        .filter_map(|(name, ip)| match ip {
            IpAddr::V4(v4) => Some((lan_score(&name, v4), v4)),
            IpAddr::V6(_) => None,
        })
        .max_by_key(|(score, ip)| (*score, std::cmp::Reverse(u32::from(*ip))))
        .map(|(_, ip)| ip)
}

/// Every address other LAN devices may use to reach this device, best first: the IPv4
/// addresses (ranked like [`lan_ipv4`], so its answer comes first), then IPv6 addresses
/// that are not link-local (global and unique-local; a link-local address needs a zone
/// that another device cannot know). Addresses of virtual-looking interfaces (Docker, VM,
/// VPN) are left out when a physical-looking interface has one. Empty if there is none.
///
/// For display (the hub screen lists them for typing into a sender); blocking (lists the
/// interfaces).
pub fn lan_addresses() -> Vec<IpAddr> {
    rank_lan_addresses(lan_candidates())
}

/// Orders `(interface name, address)` candidates for [`lan_addresses`].
fn rank_lan_addresses(candidates: Vec<(String, IpAddr)>) -> Vec<IpAddr> {
    let any_physical = candidates.iter().any(|(name, _)| !looks_virtual(name));
    let mut ranked: Vec<(u8, IpAddr)> = candidates
        .into_iter()
        .filter(|(name, _)| !any_physical || !looks_virtual(name))
        .map(|(name, ip)| match ip {
            // IPv4 before IPv6; within IPv4 the `lan_ipv4` order.
            IpAddr::V4(v4) => (4 + lan_score(&name, v4), ip),
            IpAddr::V6(_) => (0, ip),
        })
        .collect();
    ranked.sort_by(|(sa, a), (sb, b)| sb.cmp(sa).then_with(|| a.cmp(b)));
    let mut out: Vec<IpAddr> = Vec::with_capacity(ranked.len());
    for (_, ip) in ranked {
        if !out.contains(&ip) {
            out.push(ip);
        }
    }
    out
}

/// Up, non-point-to-point interfaces' usable unicast addresses with the interface name:
/// IPv4 without loopback / link-local / unspecified / multicast, IPv6 without loopback /
/// link-local (`fe80::/10`) / unspecified / multicast.
fn lan_candidates() -> Vec<(String, IpAddr)> {
    let interfaces = match if_addrs::get_if_addrs() {
        Ok(list) => list,
        Err(e) => {
            tracing::debug!(error = %e, "cannot list network interfaces");
            return Vec::new();
        }
    };
    interfaces
        .into_iter()
        .filter(|i| {
            !i.is_p2p()
                && !matches!(
                    i.oper_status,
                    if_addrs::IfOperStatus::Down
                        | if_addrs::IfOperStatus::NotPresent
                        | if_addrs::IfOperStatus::LowerLayerDown
                )
        })
        .filter_map(|i| {
            let ip = i.ip();
            let usable = match ip {
                IpAddr::V4(v4) => {
                    !v4.is_loopback()
                        && !v4.is_link_local()
                        && !v4.is_unspecified()
                        && !v4.is_multicast()
                }
                IpAddr::V6(v6) => {
                    !v6.is_loopback()
                        && !v6.is_unspecified()
                        && !v6.is_multicast()
                        && (v6.segments()[0] & 0xffc0) != 0xfe80
                }
            };
            usable.then_some((i.name, ip))
        })
        .collect()
}

/// Name prefixes of virtual network interfaces.
const VIRTUAL_PREFIXES: [&str; 14] = [
    "docker",
    "br-",
    "veth",
    "virbr",
    "vmnet",
    "vboxnet",
    "tun",
    "tap",
    "wg",
    "utun",
    "zt",
    "tailscale",
    "podman",
    "cni",
];

/// Ranks a candidate address: private ranges first, then interfaces that do not look virtual.
fn lan_score(if_name: &str, ip: Ipv4Addr) -> u8 {
    let mut score = 0;
    if ip.is_private() {
        score += 2;
    }
    if !looks_virtual(if_name) {
        score += 1;
    }
    score
}

/// Whether an interface name looks like a virtual one (container bridges, VMs, VPNs).
fn looks_virtual(if_name: &str) -> bool {
    let name = if_name.to_ascii_lowercase();
    VIRTUAL_PREFIXES.iter().any(|p| name.starts_with(p))
        || name.contains("virtual")
        || name.contains("vethernet")
        || name.contains("vpn")
}

fn close_if_expired(state: &mut State, now: u64) {
    if state
        .window
        .as_ref()
        .is_some_and(|w| w.info.expires_at_unix <= now)
    {
        state.window = None;
    }
}

/// Which [`PairMethod`] a sender uses for a user-provided secret: exactly 6 ASCII digits is a
/// PIN, anything else is a token.
pub fn method_for_secret(secret: &str) -> PairMethod {
    if secret.len() == 6 && secret.bytes().all(|b| b.is_ascii_digit()) {
        PairMethod::Pin
    } else {
        PairMethod::Token
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    fn unix_now() -> u64 {
        crate::identity::unix_now()
    }

    fn manager_with_window(expires_in: u64) -> PairingManager {
        let m = PairingManager::new([7; 32], "Desk".into(), 47810);
        m.open_window(PairingInfo {
            pin: "123456".into(),
            token: "tok-en".into(),
            uri: "hfa://pair?t=tok-en".into(),
            expires_at_unix: unix_now() + expires_in,
        });
        m
    }

    #[test]
    fn attempt_hands_out_the_right_secret() {
        let m = manager_with_window(300);
        assert!(m.begin_attempt(PairMethod::Unspecified).is_none());
        let a = m.begin_attempt(PairMethod::Pin).expect("open");
        assert_eq!(a.secret(), "123456");
        drop(a);
        let a = m.begin_attempt(PairMethod::Token).expect("open");
        assert_eq!(a.secret(), "tok-en");
    }

    #[test]
    fn only_one_attempt_in_flight() {
        let m = manager_with_window(300);
        let first = m.begin_attempt(PairMethod::Pin).expect("first");
        assert!(m.begin_attempt(PairMethod::Pin).is_none());
        assert!(m.begin_attempt(PairMethod::Token).is_none());
        drop(first);
        assert!(m.begin_attempt(PairMethod::Pin).is_some());
    }

    #[test]
    fn parallel_connections_cannot_exceed_the_guess_budget() {
        let m = Arc::new(manager_with_window(300));
        let threads = 64;
        let barrier = Arc::new(Barrier::new(threads));
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let (m, barrier) = (Arc::clone(&m), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    let mut guesses = 0;
                    barrier.wait();
                    for _ in 0..100 {
                        if let Some(attempt) = m.begin_attempt(PairMethod::Pin) {
                            guesses += 1;
                            std::thread::yield_now();
                            drop(attempt); // wrong guess
                        }
                    }
                    guesses
                })
            })
            .collect();
        let total: u32 = handles.into_iter().map(|h| h.join().expect("join")).sum();
        assert_eq!(total, MAX_FAILED_ATTEMPTS);
        assert!(m.current().is_none(), "window closed after the budget");
    }

    #[test]
    fn window_closes_after_max_failures() {
        let m = manager_with_window(300);
        for _ in 0..MAX_FAILED_ATTEMPTS {
            assert!(m.current().is_some());
            drop(m.begin_attempt(PairMethod::Pin).expect("budget left"));
        }
        assert!(m.current().is_none());
        assert!(m.begin_attempt(PairMethod::Token).is_none());
    }

    #[test]
    fn success_closes_the_window() {
        let m = manager_with_window(300);
        assert!(m.begin_attempt(PairMethod::Token).expect("open").succeed());
        assert!(m.current().is_none());
        assert!(m.begin_attempt(PairMethod::Pin).is_none());
    }

    #[test]
    fn expired_or_cancelled_window_gives_no_secret() {
        let m = manager_with_window(0);
        assert!(m.current().is_none());
        assert!(m.begin_attempt(PairMethod::Pin).is_none());
        let m = manager_with_window(300);
        m.cancel();
        assert!(m.begin_attempt(PairMethod::Pin).is_none());
    }

    #[test]
    fn stale_attempt_does_not_touch_a_new_window() {
        let m = manager_with_window(300);
        let stale = m.begin_attempt(PairMethod::Pin).expect("open");
        m.open_window(PairingInfo {
            pin: "654321".into(),
            token: "new".into(),
            uri: String::new(),
            expires_at_unix: unix_now() + 300,
        });
        let fresh = m.begin_attempt(PairMethod::Pin).expect("new window");
        // Must fail, and must not close the new window or clear its in-flight flag.
        assert!(!stale.succeed());
        assert!(m.current().is_some());
        assert!(
            m.begin_attempt(PairMethod::Pin).is_none(),
            "fresh still in flight"
        );
        drop(fresh);
        assert!(m.begin_attempt(PairMethod::Pin).is_some());
    }

    #[test]
    fn cancelled_or_expired_attempt_cannot_succeed() {
        let m = manager_with_window(300);
        let attempt = m.begin_attempt(PairMethod::Pin).expect("open");
        m.cancel();
        assert!(!attempt.succeed(), "cancel withdraws the secret");
        assert!(m.current().is_none());

        use std::sync::atomic::{AtomicU64, Ordering};
        let now = Arc::new(AtomicU64::new(1_000));
        let clock = Arc::clone(&now);
        let m = PairingManager::with_clock(
            [7; 32],
            "Desk".into(),
            47810,
            Arc::new(move || clock.load(Ordering::SeqCst)),
        );
        m.start(Duration::from_secs(60));
        let attempt = m.begin_attempt(PairMethod::Pin).expect("open");
        now.store(1_060, Ordering::SeqCst);
        assert!(!attempt.succeed(), "the window expired mid-attempt");

        // A replaced window: the old attempt fails, the new window stays open and usable.
        let m = manager_with_window(300);
        let attempt = m.begin_attempt(PairMethod::Pin).expect("open");
        let fresh = m.start(DEFAULT_PAIRING_TTL);
        assert!(!attempt.succeed());
        assert_eq!(m.current(), Some(fresh));
        assert!(m
            .begin_attempt(PairMethod::Pin)
            .expect("new window")
            .succeed());
    }

    #[test]
    fn debug_never_prints_secrets() {
        let m = manager_with_window(300);
        let info = m.current().expect("open");
        let attempt = m.begin_attempt(PairMethod::Token).expect("open");
        for text in [
            format!("{m:?}"),
            format!("{info:?}"),
            format!("{attempt:?}"),
        ] {
            assert!(
                !text.contains("123456") && !text.contains("tok-en"),
                "{text}"
            );
        }
    }

    #[test]
    fn start_builds_a_parsable_uri_and_fresh_secrets() {
        let m = PairingManager::new([7; 32], "Desk\u{7}PC".into(), 47810);
        let first = m.start(DEFAULT_PAIRING_TTL);
        assert_eq!(first.pin.len(), 6);
        assert!(first.pin.bytes().all(|b| b.is_ascii_digit()));
        let uri: hfa_proto::PairingUri = first.uri.parse().expect("valid URI");
        assert_eq!(uri.hub_id, [7; 32]);
        assert_eq!(uri.port, 47810);
        assert_eq!(uri.token, first.token);
        assert_eq!(uri.name, "DeskPC", "control characters are dropped");
        assert!(uri.host.parse::<Ipv4Addr>().is_ok(), "{}", uri.host);
        let now = unix_now();
        assert!((now + 299..=now + 301).contains(&first.expires_at_unix));
        assert_eq!(m.current(), Some(first.clone()));

        // A new window replaces the old one with new secrets and a fresh budget.
        drop(m.begin_attempt(PairMethod::Pin));
        let second = m.start(Duration::from_millis(1500));
        assert_ne!(second.token, first.token);
        assert_eq!(second.expires_at_unix, unix_now() + 2);
        for _ in 0..MAX_FAILED_ATTEMPTS {
            drop(m.begin_attempt(PairMethod::Pin).expect("fresh budget"));
        }
        assert!(m.current().is_none());
    }

    #[test]
    fn port_zero_gives_an_empty_uri_but_a_working_pin() {
        let m = PairingManager::new([7; 32], "Desk".into(), 0);
        let info = m.start(DEFAULT_PAIRING_TTL);
        assert!(info.uri.is_empty());
        assert_eq!(
            m.begin_attempt(PairMethod::Pin).expect("open").secret(),
            info.pin
        );
    }

    #[test]
    fn injected_clock_expires_the_window() {
        use std::sync::atomic::{AtomicU64, Ordering};
        let now = Arc::new(AtomicU64::new(1_000));
        let clock = Arc::clone(&now);
        let m = PairingManager::with_clock(
            [7; 32],
            "Desk".into(),
            47810,
            Arc::new(move || clock.load(Ordering::SeqCst)),
        );
        let info = m.start(Duration::from_secs(60));
        assert_eq!(info.expires_at_unix, 1_060);
        now.store(1_059, Ordering::SeqCst);
        assert!(m.begin_attempt(PairMethod::Pin).is_some());
        now.store(1_060, Ordering::SeqCst);
        assert!(m.current().is_none());
        assert!(m.begin_attempt(PairMethod::Pin).is_none());
    }

    #[test]
    fn lan_address_ranking() {
        let private = Ipv4Addr::new(192, 168, 1, 20);
        let public = Ipv4Addr::new(8, 8, 8, 8);
        assert!(lan_score("eth0", private) > lan_score("docker0", private));
        assert!(lan_score("docker0", private) > lan_score("eth0", public));
        assert!(lan_score("wlan0", private) > lan_score("vEthernet (WSL)", private));
        if let Some(ip) = lan_ipv4() {
            assert!(!ip.is_loopback() && !ip.is_link_local());
        }
    }

    #[test]
    fn secret_kind_detection() {
        assert_eq!(method_for_secret("012345"), PairMethod::Pin);
        assert_eq!(method_for_secret("12345"), PairMethod::Token);
        assert_eq!(method_for_secret("1234567"), PairMethod::Token);
        assert_eq!(method_for_secret("12a456"), PairMethod::Token);
        assert_eq!(
            method_for_secret("AAECAwQFBgcICQoLDA0ODw"),
            PairMethod::Token
        );
    }

    #[test]
    fn lan_addresses_rank_ipv4_first_and_skip_virtual_interfaces() {
        let ip = |s: &str| s.parse::<IpAddr>().expect("ip");
        let candidates = vec![
            ("docker0".to_owned(), ip("172.17.0.1")),
            ("wlan0".to_owned(), ip("2001:db8::5")),
            ("eth0".to_owned(), ip("203.0.113.9")),
            ("wlan0".to_owned(), ip("192.168.1.20")),
            ("eth1".to_owned(), ip("10.0.0.7")),
            ("eth1".to_owned(), ip("10.0.0.7")),
            ("wlan0".to_owned(), ip("fd00::20")),
        ];
        assert_eq!(
            rank_lan_addresses(candidates),
            vec![
                ip("10.0.0.7"),
                ip("192.168.1.20"),
                ip("203.0.113.9"),
                ip("2001:db8::5"),
                ip("fd00::20"),
            ]
        );
        // Only virtual interfaces: they are all there is, so they are shown.
        assert_eq!(
            rank_lan_addresses(vec![("tailscale0".to_owned(), ip("100.64.0.3"))]),
            vec![ip("100.64.0.3")]
        );
        assert!(rank_lan_addresses(Vec::new()).is_empty());
    }

    #[test]
    fn lan_addresses_start_with_the_pairing_address() {
        let all = lan_addresses();
        if let Some(best) = lan_ipv4() {
            // The best IPv4 may sit on a virtual interface only when there is no other.
            if all.first().is_some_and(IpAddr::is_ipv4) {
                assert!(all.contains(&IpAddr::V4(best)), "{all:?} / {best}");
            }
        }
        for addr in &all {
            assert!(!addr.is_loopback(), "{addr}");
            if let IpAddr::V6(v6) = addr {
                assert_ne!(v6.segments()[0] & 0xffc0, 0xfe80, "{addr}");
            }
        }
    }
}
