//! End-to-end smoke test of the flutter_rust_bridge API against the real engines: a hub and a
//! sender, both driven only through `hfa_ffi::api`, pair with the hub's PIN and stream a test
//! tone over loopback.
//!
//! The API keeps one engine manager (and so one device identity) per process, and a device
//! refuses to stream to itself, so the hub runs in a child process: this test binary started
//! again with [`HUB_DIR_ENV`] set, running only [`hub_process`]. The two processes talk over
//! the child's stdin/stdout with single lines:
//!
//! - child → parent: `HFA_E2E_HUB <port> <pin> <hub device id>` once the hub runs with an
//!   open pairing window;
//! - child → parent: `HFA_E2E_SOURCE <sender device id> <sender trusted> <label...>` once
//!   `hub_sources` lists an active source playing the tone;
//! - parent → child: `stop` after the sender stopped;
//! - child → parent: `HFA_E2E_DONE <source removed>` after `hub_stop`.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use hfa_ffi::api::app::{init_app, trusted_peers};
use hfa_ffi::api::hub::{hub_sources, hub_start, hub_start_pairing, hub_status, hub_stop};
use hfa_ffi::api::sender::{
    sender_start, sender_status, sender_stop, CaptureSourceDto, SenderStartDto,
};

/// Set (to the hub's data dir) only in the child process that runs [`hub_process`].
const HUB_DIR_ENV: &str = "HFA_FFI_E2E_HUB_DIR";
/// Label of the sender's stream, checked on the hub.
const LABEL: &str = "E2E tone";
/// Upper bound for each step (connect + pair, first audio, removal, shutdown).
const STEP_TIMEOUT: Duration = Duration::from_secs(20);

/// Writes a `settings.json` for a headless test device: a null output (the machine may have
/// no audio device) and port 0 (any free port).
fn write_settings(dir: &Path, name: &str) {
    std::fs::write(
        dir.join("settings.json"),
        format!(r#"{{"device_name": "{name}", "port": 0, "output": "Null"}}"#),
    )
    .expect("write settings");
}

/// Polls `f` every 50 ms until it returns `Some`, or panics after [`STEP_TIMEOUT`].
fn wait_for<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + STEP_TIMEOUT;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The hub side. Does nothing unless started by [`pair_stream_and_stop`] as its child.
#[test]
#[ignore = "helper: run by pair_stream_and_stop in a child process"]
fn hub_process() {
    let Some(dir) = std::env::var_os(HUB_DIR_ENV) else {
        return;
    };
    let dir = Path::new(&dir);
    write_settings(dir, "E2E hub");
    let info = init_app(dir.to_string_lossy().into_owned(), None).expect("hub init");

    let status = hub_start().expect("hub starts");
    assert!(status.running && status.port != 0, "{status:?}");
    let pairing = hub_start_pairing().expect("pairing window");
    println!(
        "HFA_E2E_HUB {} {} {}",
        status.port, pairing.pin, info.device_id
    );

    // Audible: the tone was decoded and mixed, not only announced.
    let source = wait_for("an audible source on the hub", || {
        hub_sources()
            .into_iter()
            .find(|s| s.active && s.level_db > -40.0)
    });
    let sender_trusted = trusted_peers()
        .expect("hub trusted peers")
        .iter()
        .any(|p| p.device_id == source.device_id);
    println!(
        "HFA_E2E_SOURCE {} {} {}",
        source.device_id, sender_trusted, source.label
    );

    let mut line = String::new();
    std::io::stdin().read_line(&mut line).expect("read stop");
    assert_eq!(line.trim(), "stop");
    // The sender said StreamStop / Bye, so its source goes away without waiting for
    // REMOVE_AFTER.
    let deadline = Instant::now() + STEP_TIMEOUT;
    while !hub_sources().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let removed = hub_sources().is_empty();
    hub_stop().expect("hub stops");
    assert!(!hub_status().running);
    println!("HFA_E2E_DONE {removed}");
}

/// Kills the hub child if the test fails half-way.
struct HubChild {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
}

impl HubChild {
    fn spawn(dir: &Path) -> Self {
        let exe = std::env::current_exe().expect("test binary path");
        let mut child = Command::new(exe)
            .args([
                "--exact",
                "hub_process",
                "--ignored",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(HUB_DIR_ENV, dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn the hub process");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("child stdout");
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            // libtest prints `test hub_process ... ` without a newline before the first line.
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Some(at) = line.find("HFA_E2E_") {
                    if tx.send(line[at..].to_owned()).is_err() {
                        break;
                    }
                }
            }
        });
        Self {
            child,
            stdin,
            lines,
        }
    }

    /// The fields after `tag` of the child's next protocol line.
    fn expect_line(&self, tag: &str) -> Vec<String> {
        let line = self
            .lines
            .recv_timeout(STEP_TIMEOUT)
            .unwrap_or_else(|e| panic!("no {tag} line from the hub process: {e}"));
        let mut fields = line.split(' ').map(str::to_owned);
        assert_eq!(fields.next().as_deref(), Some(tag), "{line}");
        fields.collect()
    }

    fn send(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("child stdin");
        writeln!(stdin, "{line}").expect("write to the hub process");
        stdin.flush().expect("flush");
    }

    fn wait_success(mut self) {
        self.stdin = None;
        let deadline = Instant::now() + STEP_TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().expect("wait for the hub process") {
                assert!(status.success(), "hub process failed: {status}");
                return;
            }
            assert!(Instant::now() < deadline, "hub process did not exit");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for HubChild {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

#[test]
fn pair_stream_and_stop() {
    let hub_dir = tempfile::tempdir().expect("hub tempdir");
    let sender_dir = tempfile::tempdir().expect("sender tempdir");
    let mut hub = HubChild::spawn(hub_dir.path());

    let hub_fields = hub.expect_line("HFA_E2E_HUB");
    let [port, pin, hub_id] = <[String; 3]>::try_from(hub_fields).expect("port pin id");
    let port: u16 = port.parse().expect("port");
    assert_eq!(pin.len(), 6, "{pin}");

    write_settings(sender_dir.path(), "E2E sender");
    let me = init_app(sender_dir.path().to_string_lossy().into_owned(), None).expect("init");
    assert_ne!(me.device_id, hub_id);
    assert!(trusted_peers().expect("peers").is_empty());

    sender_start(SenderStartDto {
        hub_host: "127.0.0.1".into(),
        hub_port: port,
        hub_device_id: Some(hub_id.clone()),
        hub_key: None,
        pairing_secret: Some(pin),
        source: CaptureSourceDto::Tone { freq_hz: 440.0 },
        label: LABEL.into(),
    })
    .expect("sender starts");

    let status = wait_for("the sender to stream", || {
        let s = sender_status();
        assert_ne!(s.state, "failed", "{s:?}");
        (s.state == "streaming").then_some(s)
    });
    assert_eq!(status.hub_name.as_deref(), Some("E2E hub"), "{status:?}");

    // The hub lists this device's stream with its label and trusts it after the pairing.
    let source = hub.expect_line("HFA_E2E_SOURCE");
    assert_eq!(source[0], me.device_id);
    assert_eq!(source[1], "true", "the hub trusts the sender after pairing");
    assert_eq!(source[2..].join(" "), LABEL);
    // ...and this device saved the hub.
    let peers = trusted_peers().expect("peers");
    assert!(
        peers
            .iter()
            .any(|p| p.device_id == hub_id && p.name == "E2E hub"),
        "{peers:?}"
    );

    sender_stop().expect("sender stops");
    assert_eq!(sender_status().state, "idle");
    hub.send("stop");
    assert_eq!(
        hub.expect_line("HFA_E2E_DONE"),
        ["true"],
        "the hub dropped the stopped source"
    );
    hub.wait_success();
}
