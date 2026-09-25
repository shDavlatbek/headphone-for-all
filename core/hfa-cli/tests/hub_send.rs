//! `hfa hub` and `hfa send` as separate processes: an unpaired sender is refused with a
//! clear message and a non-zero exit, a sender with the hub's PIN pairs and streams, a
//! second sender of the paired device needs no PIN, both sides show each other in their
//! output, `hfa trust` lists and removes the pairing, and SIGINT (Ctrl+C) stops everything
//! gracefully.

#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(20);

fn hfa(data_dir: &std::path::Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_hfa"));
    cmd.arg("--data-dir")
        .arg(data_dir)
        .env("RUST_LOG", "warn")
        .stdin(Stdio::null());
    cmd
}

/// A running `hfa` process whose stdout lines are collected on a thread.
struct Proc {
    child: Child,
    lines: Receiver<String>,
    seen: Vec<String>,
}

impl Proc {
    fn spawn(mut cmd: Command) -> Self {
        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn hfa");
        let stdout = child.stdout.take().expect("stdout");
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            lines,
            seen: Vec::new(),
        }
    }

    /// Waits for a stdout line containing `needle`; returns it.
    fn expect_line(&mut self, needle: &str) -> String {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(line) => {
                    eprintln!("[hfa {}] {line}", self.child.id());
                    self.seen.push(line.clone());
                    if line.contains(needle) {
                        return line;
                    }
                }
                Err(_) => panic!(
                    "no line containing {needle:?}; output so far: {:#?}",
                    self.seen
                ),
            }
        }
    }

    /// Sends SIGINT (what Ctrl+C does) and waits for the exit; returns the exit code and the
    /// rest of the output.
    fn interrupt(mut self) -> (Option<i32>, Vec<String>) {
        let status = Command::new("kill")
            .args(["-INT", &self.child.id().to_string()])
            .status()
            .expect("kill");
        assert!(status.success());
        let deadline = Instant::now() + TIMEOUT;
        let code = loop {
            if let Some(status) = self.child.try_wait().expect("wait") {
                break status.code();
            }
            assert!(Instant::now() < deadline, "hfa did not stop after SIGINT");
            std::thread::sleep(Duration::from_millis(50));
        };
        let rest: Vec<String> = self.lines.try_iter().collect();
        (code, rest)
    }
}

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn free_port() -> u16 {
    // TCP and UDP of the same number must be free; an ephemeral port that was just released
    // is free for both in practice.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    listener.local_addr().expect("addr").port()
}

#[test]
fn pairing_streaming_and_graceful_stop() {
    let hub_dir = tempfile::tempdir().unwrap();
    let sender_dir = tempfile::tempdir().unwrap();
    let port = free_port().to_string();

    let mut hub_cmd = hfa(hub_dir.path());
    hub_cmd.args([
        "hub",
        "--out",
        "null",
        "--no-mdns",
        "--pair",
        "--port",
        &port,
    ]);
    let mut hub = Proc::spawn(hub_cmd);
    hub.expect_line(&format!("on port {port}"));
    let pin_line = hub.expect_line("PIN: ");
    let pin = pin_line.rsplit("PIN: ").next().unwrap().trim().to_owned();
    assert_eq!(pin.len(), 6, "{pin_line}");
    let uri_line = hub.expect_line("hfa send --uri 'hfa://pair?");
    assert!(uri_line.contains(&format!("p={port}")), "{uri_line}");
    // The QR code follows (Unicode half blocks).
    let qr = hub.expect_line("█");
    assert!(qr.chars().count() >= 29, "{qr}");

    let to = format!("127.0.0.1:{port}");
    // Without the PIN the sender is refused with an explanation and a non-zero exit.
    let refused = hfa(sender_dir.path())
        .args(["send", "--to", &to, "--source", "tone:440"])
        .output()
        .expect("run hfa send");
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success(), "unpaired send must fail");
    assert!(
        stderr.contains("pairing required") && stderr.contains("--pin"),
        "{stderr}"
    );

    // With the PIN it pairs and streams.
    let mut send_cmd = hfa(sender_dir.path());
    send_cmd.args([
        "send",
        "--to",
        &to,
        "--pin",
        &pin,
        "--source",
        "tone:440",
        "--label",
        "Test tone",
    ]);
    let mut sender = Proc::spawn(send_cmd);
    sender.expect_line("paired with hub");
    sender.expect_line("state: streaming");
    hub.expect_line("* paired with");
    // The next pairing window opens for the next device.
    let next_pin = hub.expect_line("PIN: ");
    assert!(!next_pin.contains(&pin), "a new PIN: {next_pin}");
    hub.expect_line("\"Test tone\" connected");
    // The periodic outputs: the sender's status line, the hub's sources table.
    let status = sender.expect_line("[streaming]");
    assert!(
        status.contains("kbit/s") && status.contains("dB"),
        "{status}"
    );
    let row = hub.expect_line("Test tone");
    assert!(row.contains("active"), "{row}");

    // The running hub keeps its own copy of the trusted devices, so `trust remove` refuses
    // to change them behind its back.
    let busy = hfa(hub_dir.path())
        .args(["trust", "remove", "ab12-cd34-ef56-7890"])
        .output()
        .expect("trust remove");
    assert!(!busy.status.success(), "trust remove while the hub runs");
    let stderr = String::from_utf8_lossy(&busy.stderr);
    assert!(stderr.contains("running `hfa hub`"), "{stderr}");

    // Paired now: a second sender on the same device needs no PIN.
    let mut second_cmd = hfa(sender_dir.path());
    second_cmd.args([
        "send",
        "--to",
        &to,
        "--source",
        "tone:1000",
        "--label",
        "Second",
    ]);
    let mut second = Proc::spawn(second_cmd);
    second.expect_line("state: streaming");
    assert!(
        !second.seen.iter().any(|l| l.contains("paired with")),
        "{:#?}",
        second.seen
    );
    hub.expect_line("\"Second\" connected");
    let (code, _) = second.interrupt();
    assert_eq!(code, Some(0));

    let (code, rest) = sender.interrupt();
    assert_eq!(code, Some(0), "sender exit code; output {rest:#?}");
    assert!(rest.iter().any(|l| l.starts_with("Stopped (")), "{rest:#?}");
    hub.expect_line("removed");
    let (code, rest) = hub.interrupt();
    assert_eq!(code, Some(0), "hub exit code; output {rest:#?}");
    assert!(rest.iter().any(|l| l == "Hub stopped."), "{rest:#?}");

    // Both sides remember the pairing.
    for dir in [hub_dir.path(), sender_dir.path()] {
        let out = hfa(dir)
            .args(["trust", "list"])
            .output()
            .expect("trust list");
        assert!(out.status.success());
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.starts_with("DEVICE ID"), "{text}");
        assert_eq!(text.lines().count(), 2, "{text}");
    }
    // Forgetting the hub works once.
    let list = hfa(sender_dir.path())
        .args(["trust", "list"])
        .output()
        .unwrap();
    let hub_id = String::from_utf8_lossy(&list.stdout)
        .lines()
        .nth(1)
        .and_then(|l| l.split_whitespace().next())
        .unwrap()
        .to_owned();
    let removed = hfa(sender_dir.path())
        .args(["trust", "remove", &hub_id])
        .output()
        .unwrap();
    assert!(removed.status.success());
    let again = hfa(sender_dir.path())
        .args(["trust", "remove", &hub_id])
        .output()
        .unwrap();
    assert!(!again.status.success(), "removing twice fails");
}

#[test]
fn wrong_pins_close_pairing_for_good() {
    let hub_dir = tempfile::tempdir().unwrap();
    let sender_dir = tempfile::tempdir().unwrap();
    let port = free_port().to_string();
    let mut hub_cmd = hfa(hub_dir.path());
    hub_cmd.args([
        "hub",
        "--out",
        "null",
        "--no-mdns",
        "--pair",
        "--port",
        &port,
    ]);
    let mut hub = Proc::spawn(hub_cmd);
    let pin_line = hub.expect_line("PIN: ");
    let pin = pin_line.rsplit("PIN: ").next().unwrap().trim().to_owned();
    let wrong = if pin == "000000" { "111111" } else { "000000" };
    let to = format!("127.0.0.1:{port}");
    let send = |pin: &str| {
        hfa(sender_dir.path())
            .args(["send", "--to", &to, "--pin", pin, "--source", "tone:440"])
            .output()
            .expect("run hfa send")
    };
    // The core's guess budget: 5 attempts per window.
    for _ in 0..5 {
        assert!(!send(wrong).status.success(), "a wrong PIN must fail");
    }
    let closed = hub.expect_line("Pairing closed after");
    assert!(closed.contains("failed attempt"), "{closed}");
    // Not reopened: the displayed PIN no longer works, and no new PIN is shown even after
    // a few refreshes.
    let late = send(&pin);
    assert!(
        !late.status.success(),
        "the old PIN after the budget closed"
    );
    hub.expect_line("--- sources ---");
    hub.expect_line("--- sources ---");
    let seen = hub.seen.clone();
    let (code, rest) = hub.interrupt();
    assert_eq!(code, Some(0));
    let all: Vec<&String> = seen.iter().chain(&rest).collect();
    let pins = all.iter().filter(|l| l.contains("PIN: ")).count();
    assert_eq!(pins, 1, "no second pairing window: {all:#?}");
}

#[test]
fn unknown_hosts_and_hubs_fail_fast() {
    let dir = tempfile::tempdir().unwrap();
    let out = hfa(dir.path())
        .args([
            "send",
            "--to",
            "no-such-hub.invalid",
            "--source",
            "tone:440",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("hub not found"), "{stderr}");
}
