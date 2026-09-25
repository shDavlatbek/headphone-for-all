//! `hfa hub`: runs a [`HubEngine`], shows the pairing PIN/URI/QR code, a live sources table
//! (refreshed every second) and the hub's events, and stops gracefully on Ctrl+C.
//!
//! On a terminal the dashboard is redrawn in place (header, pairing, table, recent events);
//! otherwise (pipe, file, CI log) events are printed as they happen and the table is appended
//! once per second.

use std::collections::VecDeque;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context;
use hfa_core::{HubConfig, HubEngine, HubEvent, HubHandle, PairingInfo, Settings};
use tokio::sync::broadcast::error::RecvError;

use crate::cli::HubArgs;
use crate::commands::{blocking, DataDirLock};
use crate::display::{qr_text, sources_table};

/// Device buffer requested from the output (same as the app).
const OUTPUT_BUFFER_MS: u32 = 20;
/// Refresh period of the sources table.
const REFRESH: Duration = Duration::from_secs(1);
/// Events kept below the table on a terminal.
const RECENT_EVENTS: usize = 8;

/// Runs `hfa hub` until Ctrl+C.
pub async fn run(data_dir: PathBuf, args: HubArgs) -> anyhow::Result<()> {
    // Held for the whole run: `hfa trust remove` must not edit the trust store behind the
    // engine's back.
    let _lock = DataDirLock::shared(data_dir.clone()).await?;
    let dir = data_dir.clone();
    let mut settings = blocking(move || Settings::load_or_default(&dir))
        .await?
        .with_context(|| format!("cannot load the settings from {}", data_dir.display()))?;
    if let Some(port) = args.port {
        settings.port = port;
    }
    let target = args.out.clone().unwrap_or_else(|| settings.output.clone());
    let shown_target = target.to_string();
    let output = blocking(move || hfa_capture::open_output(&target, OUTPUT_BUFFER_MS))
        .await?
        .with_context(|| format!("cannot open the output {shown_target:?}"))?;
    let name = settings.device_name.clone();
    let hub = HubEngine::start(HubConfig {
        settings,
        output,
        advertise: !args.no_mdns,
    })
    .await
    .context("cannot start the hub")?;

    let header = format!(
        "Hub {name:?} ({id}) on port {port} (TCP control + UDP media), output {shown_target}, \
         mDNS {mdns}.\nSenders: hfa send --to <this host>:{port}{hint}   (Ctrl+C to stop)",
        id = hub.device_id(),
        port = hub.local_port(),
        mdns = if args.no_mdns { "off" } else { "on" },
        hint = if args.no_mdns {
            String::new()
        } else {
            format!("  or  hfa send --hub {:?}", name)
        },
    );
    let mut dash = Dashboard::new(header);
    let mut pairing = PairingWatch::default();
    if args.pair {
        pairing.open(&hub, &mut dash);
    }

    let result = event_loop(&hub, &mut dash, &mut pairing, args.pair).await;
    dash.event("Stopping the hub...".to_owned());
    dash.finish();
    stop_or_force(hub.stop()).await;
    println!("Hub stopped.");
    result
}

/// Awaits a graceful stop; a second Ctrl+C during it exits the process at once (the first one
/// replaced the default Ctrl+C handler, so without this a hung stop could only be killed).
pub async fn stop_or_force(stop: impl std::future::Future<Output = ()>) {
    tokio::select! {
        () = stop => {}
        r = tokio::signal::ctrl_c() => {
            if r.is_ok() {
                eprintln!("Interrupted again: exiting without a clean stop.");
                std::process::exit(130);
            }
            // No signal handler: nothing can interrupt the stop, so just wait for it.
        }
    }
}

/// Handles events and refreshes until Ctrl+C.
async fn event_loop(
    hub: &HubHandle,
    dash: &mut Dashboard,
    pairing: &mut PairingWatch,
    keep_pairing: bool,
) -> anyhow::Result<()> {
    let mut events = hub.events();
    let mut tick = tokio::time::interval(REFRESH);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    loop {
        tokio::select! {
            r = &mut ctrl_c => {
                r.context("cannot listen for Ctrl+C")?;
                return Ok(());
            }
            ev = events.recv() => match ev {
                Ok(ev) => {
                    if let Some(line) = describe_event(&ev) {
                        dash.event(line);
                    }
                    match ev {
                        // A pairing window is single-use: open the next one for the next
                        // device (only a device that knew the secret can trigger this).
                        HubEvent::PairingCompleted { .. } if keep_pairing => {
                            pairing.open(hub, dash);
                        }
                        HubEvent::PairingFailed { .. } => pairing.failed(),
                        _ => {}
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    // A skipped event may have been a failed attempt: count one.
                    pairing.failed();
                    dash.event(format!("({n} events skipped)"));
                }
                Err(RecvError::Closed) => anyhow::bail!("the hub stopped unexpectedly"),
            },
            _ = tick.tick() => {
                pairing.check(hub, dash, unix_now());
                dash.refresh(&sources_table(&hub.sources()));
            }
        }
    }
}

/// What `hfa hub --pair` does about the window it shows (see [`PairingWatch::decide`]).
#[derive(Debug, Clone, PartialEq, Eq)]
enum PairingDecision {
    /// The window is still open.
    Keep,
    /// The window expired unused (no failed attempt): open a new one.
    Reopen,
    /// Closed before its expiry without a failed attempt: a pairing is completing (its
    /// `PairingCompleted` event is on the way) or attempts were aborted before a guess could
    /// be checked. Wait until the original expiry.
    Wait,
    /// Closed (expired or guess budget used up) after failed attempts: not reopened, so a
    /// guesser never gets a fresh budget without the operator's action.
    Closed { failures: u32 },
}

/// The pairing window shown by `hfa hub --pair` and the failed attempts seen during it.
#[derive(Default)]
struct PairingWatch {
    /// The window on screen (`None`: none, or closed for good).
    shown: Option<PairingInfo>,
    /// `PairingFailed` events since `shown` opened (a lagged event stream counts as one).
    failures: u32,
    /// Consecutive checks that found the window closed early (see [`PairingDecision::Wait`]);
    /// the "closed early" text shows from the second one, so a pairing whose
    /// `PairingCompleted` event is a moment late does not flash it.
    waiting: u32,
}

impl PairingWatch {
    /// Opens a new window (fresh PIN/token and guess budget) and shows it.
    fn open(&mut self, hub: &HubHandle, dash: &mut Dashboard) {
        let info = hub.start_pairing();
        dash.set_pairing(Some(pairing_text(&info)));
        self.shown = Some(info);
        self.failures = 0;
        self.waiting = 0;
    }

    /// Counts a failed pairing attempt against the shown window.
    fn failed(&mut self) {
        if self.shown.is_some() {
            self.failures = self.failures.saturating_add(1);
        }
    }

    /// Compares the shown window with the hub's (once per second) and acts on it.
    fn check(&mut self, hub: &HubHandle, dash: &mut Dashboard, now: u64) {
        let Some(shown) = &self.shown else {
            return;
        };
        match Self::decide(shown, self.failures, hub.current_pairing().as_ref(), now) {
            PairingDecision::Keep => {}
            PairingDecision::Reopen => {
                dash.event("The pairing window expired unused; opening a new one.".to_owned());
                self.open(hub, dash);
            }
            PairingDecision::Wait => {
                self.waiting = self.waiting.saturating_add(1);
                if self.waiting == 2 {
                    let minutes = shown.expires_at_unix.saturating_sub(now).div_ceil(60);
                    dash.set_pairing(Some(format!(
                        "Pairing window closed early (a device is completing its pairing, or \
                         attempts were aborted); a new one opens in {minutes} min.\n"
                    )));
                }
            }
            PairingDecision::Closed { failures } => {
                self.shown = None;
                self.waiting = 0;
                dash.set_pairing(Some(closed_text(failures)));
            }
        }
    }

    /// The policy: reopen only after a completed pairing (handled on the event) or after a
    /// window that expired with no failed attempt; after any failed attempt the window stays
    /// closed, so the core's per-window guess budget bounds guessing for the whole run.
    fn decide(
        shown: &PairingInfo,
        failures: u32,
        current: Option<&PairingInfo>,
        now: u64,
    ) -> PairingDecision {
        if current.is_some_and(|c| c.token == shown.token) {
            PairingDecision::Keep
        } else if failures > 0 {
            PairingDecision::Closed { failures }
        } else if now >= shown.expires_at_unix {
            PairingDecision::Reopen
        } else {
            PairingDecision::Wait
        }
    }
}

/// The pairing block once a window closed after failed attempts.
fn closed_text(failures: u32) -> String {
    format!(
        "Pairing closed after {failures} failed attempt(s) (wrong PIN/token); it is not \
         reopened automatically. Restart `hfa hub --pair` to pair another device.\n"
    )
}

/// The pairing block: PIN, URI, QR code and expiry.
fn pairing_text(info: &PairingInfo) -> String {
    let minutes = info.expires_at_unix.saturating_sub(unix_now()).div_ceil(60);
    let mut text = format!(
        "Pairing open for {minutes} min (one device). PIN: {}\n",
        info.pin
    );
    if info.uri.is_empty() {
        text.push_str("(no pairing URI: the hub's address is unknown; use the PIN)\n");
        return text;
    }
    text.push_str(&format!(
        "Sender: hfa send --uri '{}'\n   or: hfa send --to <this host> --pin {}\n",
        info.uri, info.pin
    ));
    if let Some(qr) = qr_text(&info.uri) {
        text.push_str("Or scan with the app:\n");
        text.push_str(&qr);
        text.push('\n');
    }
    text
}

/// One line for a hub event (`None` for the per-second updates).
pub fn describe_event(ev: &HubEvent) -> Option<String> {
    Some(match ev {
        HubEvent::SourceAdded(s) => format!(
            "+ {} / {:?} connected ({}, stream {})",
            s.device_name, s.label, s.platform, s.stream_id
        ),
        HubEvent::SourceRemoved { stream_id } => format!("- stream {stream_id} removed"),
        HubEvent::SourceUpdated(_) => return None,
        HubEvent::PairingCompleted { device_id, name } => {
            format!("* paired with {name:?} ({device_id}); it is now trusted")
        }
        HubEvent::PairingFailed { reason } => format!("! pairing failed: {reason}"),
        HubEvent::Error(message) => format!("! {message}"),
    })
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Whether stdout understands ANSI cursor control.
fn ansi_terminal() -> bool {
    crate::display::vt_console(std::io::stdout().is_terminal())
}

/// The hub's console output.
struct Dashboard {
    /// Redraw in place (terminal) instead of appending.
    live: bool,
    header: String,
    pairing: Option<String>,
    /// Pairing block not yet printed (append mode).
    pairing_dirty: bool,
    table: String,
    recent: VecDeque<String>,
    /// The screen was cleared once (terminal).
    cleared: bool,
}

impl Dashboard {
    fn new(header: String) -> Self {
        let dash = Self {
            live: ansi_terminal(),
            header,
            pairing: None,
            pairing_dirty: false,
            table: String::new(),
            recent: VecDeque::new(),
            cleared: false,
        };
        if !dash.live {
            println!("{}", dash.header);
        }
        dash
    }

    fn set_pairing(&mut self, text: Option<String>) {
        self.pairing = text;
        self.pairing_dirty = true;
        self.draw();
    }

    fn event(&mut self, line: String) {
        if self.live {
            if self.recent.len() == RECENT_EVENTS {
                self.recent.pop_front();
            }
            self.recent.push_back(line);
            self.draw();
        } else {
            println!("{line}");
        }
    }

    fn refresh(&mut self, table: &str) {
        table.clone_into(&mut self.table);
        if self.live {
            self.draw();
        } else {
            self.draw();
            println!("--- sources ---\n{}", self.table.trim_end());
        }
    }

    /// Terminal: redraws everything from the top-left corner (each line clears its rest, then
    /// everything below is cleared, so there is no flicker). Append mode: prints a new
    /// pairing block.
    fn draw(&mut self) {
        let mut out = std::io::stdout().lock();
        if !self.live {
            if self.pairing_dirty {
                if let Some(p) = &self.pairing {
                    let _ = writeln!(out, "{}", p.trim_end());
                }
                self.pairing_dirty = false;
            }
            let _ = out.flush();
            return;
        }
        let mut text = String::new();
        text.push_str(&self.header);
        text.push_str("\n\n");
        if let Some(p) = &self.pairing {
            text.push_str(p);
            text.push('\n');
        }
        text.push_str(&self.table);
        if !self.recent.is_empty() {
            text.push_str("\nEvents:\n");
            for line in &self.recent {
                text.push_str(line);
                text.push('\n');
            }
        }
        if !self.cleared {
            let _ = write!(out, "\x1b[2J");
            self.cleared = true;
        }
        let _ = write!(out, "\x1b[H");
        for line in text.lines() {
            let _ = writeln!(out, "{line}\x1b[K");
        }
        let _ = write!(out, "\x1b[J");
        let _ = out.flush();
    }

    /// Leaves the terminal in append mode below the dashboard.
    fn finish(&mut self) {
        if self.live {
            self.live = false;
            println!();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_events() {
        assert_eq!(
            describe_event(&HubEvent::SourceRemoved { stream_id: 9 }).as_deref(),
            Some("- stream 9 removed")
        );
        let paired = describe_event(&HubEvent::PairingCompleted {
            device_id: "ab12-cd34-ef56-7890".into(),
            name: "Phone".into(),
        })
        .unwrap();
        assert!(paired.contains("Phone") && paired.contains("ab12-cd34-ef56-7890"));
        assert!(describe_event(&HubEvent::SourceUpdated(sample_source())).is_none());
        let added = describe_event(&HubEvent::SourceAdded(sample_source())).unwrap();
        assert!(added.contains("Laptop") && added.contains("stream 3"));
    }

    #[test]
    fn pairing_block_shows_pin_uri_and_qr() {
        let info = PairingInfo {
            pin: "123456".into(),
            token: "tok".into(),
            uri: "hfa://pair?v=0&h=10.0.0.2&p=47810&id=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA&t=tok&n=Hub".into(),
            expires_at_unix: unix_now() + 300,
        };
        let text = pairing_text(&info);
        assert!(text.contains("PIN: 123456"));
        assert!(text.contains("for 5 min"));
        assert!(text.contains(&info.uri));
        assert!(text.contains('█') || text.contains('▀') || text.contains('▄'));
        let no_uri = pairing_text(&PairingInfo {
            uri: String::new(),
            ..info
        });
        assert!(no_uri.contains("use the PIN"));
    }

    fn window(token: &str, expires_at_unix: u64) -> PairingInfo {
        PairingInfo {
            pin: "123456".into(),
            token: token.into(),
            uri: String::new(),
            expires_at_unix,
        }
    }

    #[test]
    fn pairing_policy_never_renews_a_guessed_window() {
        let shown = window("a", 1000);
        let decide = PairingWatch::decide;
        // Still open.
        assert_eq!(decide(&shown, 0, Some(&shown), 10), PairingDecision::Keep);
        assert_eq!(decide(&shown, 3, Some(&shown), 10), PairingDecision::Keep);
        // Expired unused: renewed.
        assert_eq!(decide(&shown, 0, None, 1000), PairingDecision::Reopen);
        // Closed by the guess budget, or expired after failed attempts: closed for good,
        // also long after the original expiry.
        assert_eq!(
            decide(&shown, 5, None, 10),
            PairingDecision::Closed { failures: 5 }
        );
        assert_eq!(
            decide(&shown, 1, None, 5000),
            PairingDecision::Closed { failures: 1 }
        );
        // Closed early without a failed guess (a pairing completing): wait for its event,
        // renew only at the original expiry.
        assert_eq!(decide(&shown, 0, None, 10), PairingDecision::Wait);
        // Another window replaced it (someone else opened one): not ours to keep.
        let other = window("b", 2000);
        assert_eq!(decide(&shown, 0, Some(&other), 10), PairingDecision::Wait);
        assert!(closed_text(5).contains("5 failed attempt"));
    }

    fn sample_source() -> hfa_core::SourceInfo {
        hfa_core::SourceInfo {
            stream_id: 3,
            device_id: "ab12-cd34-ef56-7890".into(),
            device_name: "Laptop".into(),
            label: "System audio".into(),
            platform: "linux".into(),
            gain: 1.0,
            muted: false,
            priority: false,
            active: true,
            stats: hfa_core::StreamStats::default(),
        }
    }
}
