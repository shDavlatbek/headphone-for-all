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
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

/// Hub-side pairing state. `Send + Sync`; share it with `Arc`. `Debug` never prints secrets.
pub struct PairingManager {
    hub_id: [u8; 32],
    name: String,
    port: u16,
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
        Self {
            hub_id,
            name,
            port,
            state: parking_lot::Mutex::new(State::default()),
        }
    }

    /// Opens (or replaces) a pairing window valid for `ttl`, with a fresh PIN and token and
    /// a zero attempt counter.
    pub fn start(&self, _ttl: Duration) -> PairingInfo {
        let _ = (&self.hub_id, &self.name, self.port);
        todo!("feat/core-engine")
    }

    /// Closes the current window.
    pub fn cancel(&self) {
        self.state.lock().window = None;
    }

    /// The open window, if any and not expired.
    pub fn current(&self) -> Option<PairingInfo> {
        let now = unix_now();
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
    /// the peer's confirmation MAC verified; dropping the guard in any other way (wrong
    /// secret, error, disconnect, timeout) is a failed attempt, and the window closes once
    /// the limit is reached.
    pub fn begin_attempt(&self, method: PairMethod) -> Option<PairingAttempt<'_>> {
        let now = unix_now();
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
            succeeded: false,
        })
    }

    /// Installs `info` as the open window (fresh counter, new generation). `start` builds the
    /// PIN, token and URI and then calls this.
    // Only the tests call it until `start` is implemented by feat/core-engine.
    #[cfg_attr(not(test), allow(dead_code))]
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

    /// Ends the attempt of window `generation`.
    fn finish_attempt(&self, generation: u64, succeeded: bool) {
        let mut state = self.state.lock();
        let Some(window) = state.window.as_mut() else {
            return;
        };
        if window.generation != generation {
            // The window was cancelled or replaced meanwhile; leave the new one alone.
            return;
        }
        window.in_flight = false;
        if succeeded || window.attempts >= MAX_FAILED_ATTEMPTS {
            // Success: PIN and token are one-time. Failure: guess budget used up.
            state.window = None;
        }
    }
}

/// One in-flight pairing attempt (see [`PairingManager::begin_attempt`]). Dropping it
/// without calling [`PairingAttempt::succeed`] records a failure.
pub struct PairingAttempt<'a> {
    manager: &'a PairingManager,
    generation: u64,
    secret: String,
    succeeded: bool,
}

impl PairingAttempt<'_> {
    /// The PIN or token to run SPAKE2 with.
    pub fn secret(&self) -> &str {
        &self.secret
    }

    /// The peer proved knowledge of the secret: closes the window (one-time secrets).
    pub fn succeed(mut self) {
        self.succeeded = true;
        // Drop runs `finish_attempt(.., true)`.
    }
}

impl Drop for PairingAttempt<'_> {
    fn drop(&mut self) {
        self.manager.finish_attempt(self.generation, self.succeeded);
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

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
        m.begin_attempt(PairMethod::Token).expect("open").succeed();
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
        stale.succeed(); // must not close the new window or clear its in-flight flag
        assert!(m.current().is_some());
        assert!(
            m.begin_attempt(PairMethod::Pin).is_none(),
            "fresh still in flight"
        );
        drop(fresh);
        assert!(m.begin_attempt(PairMethod::Pin).is_some());
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
}
