//! Runs the built `hfa selftest` binary: an in-process hub, two tone senders and the
//! network impairment proxy, checked by the binary's own WAV analysis (exit code 0 = every
//! tone present, glitches within budget, latency measured).

use std::process::{Command, Output};
use std::sync::Mutex;

/// The selftests run real-time audio threads; running two at once on a small machine would
/// only add scheduling noise.
static SERIAL: Mutex<()> = Mutex::new(());

fn selftest(args: &[&str]) -> (Output, String) {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let output = Command::new(env!("CARGO_BIN_EXE_hfa"))
        .arg("selftest")
        .args(args)
        .env("RUST_LOG", "warn")
        .output()
        .expect("run hfa");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    eprintln!("{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
    (output, stdout)
}

/// The latency line of the report, in ms.
fn latency_ms(report: &str) -> f64 {
    let line = report
        .lines()
        .find(|l| l.contains("end-to-end latency"))
        .expect("latency line");
    let value = line
        .split("output): ")
        .nth(1)
        .and_then(|rest| rest.split(" ms").next())
        .expect("latency value");
    value.parse().expect("number")
}

#[test]
fn clean_network_passes() {
    let (output, report) = selftest(&["--seconds", "3", "--seed", "11"]);
    assert!(output.status.success(), "selftest failed");
    assert!(report.contains("Result: PASS"));
    assert!(report.contains("tone    440 Hz") && report.contains("tone   1000 Hz"));
    assert!(report.contains("dropped 0 (0.0 %)"), "no loss configured");
    let latency = latency_ms(&report);
    assert!((20.0..300.0).contains(&latency), "latency {latency} ms");
}

#[test]
fn lossy_jittery_network_passes() {
    let (output, report) = selftest(&[
        "--seconds",
        "3",
        "--loss",
        "5",
        "--jitter",
        "20",
        "--seed",
        "12",
    ]);
    assert!(output.status.success(), "selftest failed");
    assert!(report.contains("Result: PASS"));
    // The proxy really dropped packets and the hub recovered some of them.
    let network = report
        .lines()
        .find(|l| l.starts_with("network:"))
        .expect("network line");
    assert!(!network.contains("dropped 0 "), "{network}");
    let latency = latency_ms(&report);
    assert!((20.0..400.0).contains(&latency), "latency {latency} ms");
}

#[test]
fn heavy_loss_fails_with_a_non_zero_exit() {
    // 60 % loss cannot be hidden: the command must report the failure and exit non-zero.
    let (output, report) = selftest(&[
        "--seconds",
        "2",
        "--loss",
        "60",
        "--senders",
        "1",
        "--seed",
        "13",
    ]);
    assert!(!output.status.success(), "selftest should fail");
    assert!(report.contains("Result: FAIL"), "{report}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("selftest failed"), "{stderr}");
}
