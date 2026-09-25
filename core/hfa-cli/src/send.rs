//! `hfa send`: captures this device's audio and streams it to a hub with a [`SenderEngine`].
//!
//! Hub resolution: `--uri` supplies host, port, the hub key (`expected_hub_key`) and the
//! one-time token (`pairing_secret`); `--to` overrides host/port; `--hub` looks the hub up
//! over mDNS (up to [`DISCOVER_TIMEOUT`], trusted hubs preferred) before the engine starts,
//! so a hub that cannot be found is a clear error instead of an endless reconnect loop.
//! State changes are printed as they happen and a status line every 2 s; pairing and key
//! errors end the command with a non-zero exit code and an explanation.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, bail, Context};
use hfa_capture::CaptureTarget;
use hfa_core::sender::DISCOVER_TIMEOUT;
use hfa_core::{
    CoreError, DiscoveryEvent, HubAddress, HubInfo, SenderConfig, SenderEngine, SenderEvent,
    SenderHandle, SenderState, Settings, TrustStore,
};
use tokio::sync::broadcast::error::RecvError;

use crate::cli::SendArgs;
use crate::commands::{blocking, hub_addrs};
use crate::display::{level, millis, percent};

/// Interval of the status line.
const STATUS_INTERVAL: Duration = Duration::from_secs(2);
/// How often the state is polled (state changes are printed from it).
const STATE_POLL: Duration = Duration::from_millis(200);
/// After an untrusted name match, how much longer to wait for a trusted hub of that name.
const TRUSTED_GRACE: Duration = Duration::from_secs(1);

/// Where and how to connect, resolved from the arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    hub: HubAddress,
    expected_hub_key: Option<[u8; 32]>,
    secret: Option<String>,
    /// For messages.
    shown: String,
}

/// Runs `hfa send` until Ctrl+C or a fatal error.
pub async fn run(data_dir: PathBuf, args: SendArgs) -> anyhow::Result<()> {
    let dir = data_dir.clone();
    let (settings, trust) = blocking(move || -> hfa_core::Result<_> {
        Ok((Settings::load_or_default(&dir)?, TrustStore::load(&dir)?))
    })
    .await?
    .with_context(|| format!("cannot load the settings from {}", data_dir.display()))?;
    let mut settings = settings;
    if let Some(bitrate) = args.bitrate {
        settings.bitrate = bitrate;
    }
    if let Some(frame_ms) = args.frame_ms {
        settings.frame_ms = frame_ms;
    }

    let target = resolve_target(&args, &trust).await?;
    let label = match &args.label {
        Some(l) if !l.trim().is_empty() => l.trim().to_owned(),
        _ => default_label(&args.source).await,
    };
    let source = args.source.clone();
    let (capture, warning) = blocking(move || hfa_core::sender::open_capture(&source))
        .await?
        .with_context(|| format!("cannot open the capture source {}", args.source))?;
    if let Some(w) = warning {
        println!("warning: {w}");
    }
    println!(
        "Streaming {label:?} ({}) to {}: {} kbit/s, {} ms frames. Ctrl+C to stop.",
        capture.describe(),
        target.shown,
        settings.bitrate / 1000,
        settings.frame_ms
    );
    let sender = SenderEngine::start(SenderConfig {
        hub: target.hub,
        settings,
        capture,
        label,
        expected_hub_key: target.expected_hub_key,
        pairing_secret: target.secret,
    })
    .await
    .context("cannot start the sender")?;

    let outcome = watch(&sender).await;
    let failed = outcome.err();
    if failed.is_none() {
        println!("Stopping...");
    }
    let (audio, keepalives) = sender.packets_sent();
    sender.stop().await;
    match failed {
        None => {
            println!("Stopped ({audio} audio packets, {keepalives} keep-alives sent).");
            Ok(())
        }
        Some(e) => Err(e),
    }
}

/// Prints events, state changes and status lines until Ctrl+C (`Ok`) or a final failure
/// (`Err` with an explanation).
async fn watch(sender: &SenderHandle) -> anyhow::Result<()> {
    let mut events = sender.events();
    let mut poll = tokio::time::interval(STATE_POLL);
    let mut status = tokio::time::interval(STATUS_INTERVAL);
    status.reset(); // first status line after 2 s
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);
    let mut last_state: Option<SenderState> = None;
    // Error events wait for the next state poll: the error that ends the sender is reported
    // once, as the command's error, not also as a warning.
    let mut pending_errors: Vec<String> = Vec::new();
    loop {
        tokio::select! {
            r = &mut ctrl_c => {
                r.context("cannot listen for Ctrl+C")?;
                return Ok(());
            }
            ev = events.recv() => match ev {
                Ok(SenderEvent::Error(message)) => pending_errors.push(message),
                Ok(ev) => {
                    if let Some(line) = describe_event(&ev) {
                        println!("{line}");
                    }
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => {}
            },
            _ = poll.tick() => {
                let state = sender.status().state;
                for message in pending_errors.drain(..) {
                    if !matches!(&state, SenderState::Failed(m) if *m == message) {
                        println!("warning: {message}");
                    }
                }
                if last_state.as_ref() != Some(&state) {
                    match &state {
                        SenderState::Failed(message) => return Err(explain_failure(message)),
                        SenderState::Stopped => bail!("the sender stopped unexpectedly"),
                        other => println!("state: {}", state_name(other)),
                    }
                    last_state = Some(state);
                }
            }
            _ = status.tick() => println!("{}", status_line(sender)),
        }
    }
}

/// Resolves `--uri`, `--to`, `--hub` and `--pin` into an engine address, key and secret.
async fn resolve_target(args: &SendArgs, trust: &TrustStore) -> anyhow::Result<Target> {
    let uri = args.auth.uri.as_ref();
    let expected_hub_key = uri.map(|u| u.hub_id);
    let secret = args
        .auth
        .pin
        .clone()
        .or_else(|| uri.map(|u| u.token.clone()));
    if let Some(name_or_id) = &args.dest.hub {
        let info = find_hub(name_or_id, trust).await?;
        let key = expected_hub_key.or_else(|| trust.get(&info.device_id).map(|p| p.public_key));
        let shown = format!(
            "{:?} ({}) at {}",
            info.name,
            info.device_id,
            hub_addrs(&info)
        );
        // By device id: the engine re-discovers it on every reconnect (the address may
        // change) and checks that the hub's key has this fingerprint.
        return Ok(Target {
            hub: HubAddress::Discover {
                name_or_id: info.device_id,
            },
            expected_hub_key: key,
            secret,
            shown,
        });
    }
    let (host, port) = match (&args.dest.to, uri) {
        (Some(to), _) => (to.host.clone(), to.port),
        (None, Some(u)) => (u.host.clone(), u.port),
        (None, None) => bail!("say where the hub is: --to <host[:port]>, --hub <name> or --uri"),
    };
    // Fail fast on a host name that does not resolve (the engine would retry forever).
    let lookup = host.clone();
    let resolved = tokio::net::lookup_host((lookup.as_str(), port)).await;
    match resolved {
        Ok(mut addrs) => {
            if addrs.next().is_none() {
                return Err(hub_not_found(&host, "the name has no address"));
            }
        }
        Err(e) => return Err(hub_not_found(&host, &e.to_string())),
    }
    let shown = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    Ok(Target {
        hub: HubAddress::Direct { host, port },
        expected_hub_key,
        secret,
        shown,
    })
}

fn hub_not_found(what: &str, why: &str) -> anyhow::Error {
    anyhow!(
        "{}: {why}. Check the address, or use `hfa discover` to list the hubs on this network.",
        CoreError::HubNotFound(what.to_owned())
    )
}

/// Browses mDNS for a hub by device id or name (case-insensitive), preferring trusted hubs
/// when several share a name.
async fn find_hub(name_or_id: &str, trust: &TrustStore) -> anyhow::Result<HubInfo> {
    let target = name_or_id.trim();
    let by_id = hfa_proto::is_fingerprint(target);
    let wanted = target.to_lowercase();
    println!(
        "Looking for hub {target:?} on the local network (up to {} s)...",
        DISCOVER_TIMEOUT.as_secs()
    );
    let mut browser = hfa_core::browse().context("cannot start mDNS browsing")?;
    let mut deadline = tokio::time::Instant::now() + DISCOVER_TIMEOUT;
    let mut best: Option<HubInfo> = None;
    let mut seen: Vec<String> = Vec::new();
    while let Ok(Some(event)) = tokio::time::timeout_at(deadline, browser.recv()).await {
        let DiscoveryEvent::Found(info) = event else {
            continue;
        };
        let entry = format!("{:?} ({})", info.name, info.device_id);
        if !seen.contains(&entry) {
            seen.push(entry);
        }
        let matches = if by_id {
            info.device_id == target
        } else {
            info.name.to_lowercase() == wanted
        };
        if !matches || info.addrs.is_empty() {
            continue;
        }
        let trusted = trust.get(&info.device_id).is_some();
        if by_id || trusted {
            best = Some(info);
            break;
        }
        if best.is_none() {
            deadline = deadline.min(tokio::time::Instant::now() + TRUSTED_GRACE);
            best = Some(info);
        }
    }
    best.ok_or_else(|| {
        let found = if seen.is_empty() {
            "no hub answered (is multicast DNS blocked? try --to <ip>)".to_owned()
        } else {
            format!("hubs seen: {}", seen.join(", "))
        };
        anyhow!(
            "{} within {} s; {found}",
            CoreError::HubNotFound(target.to_owned()),
            DISCOVER_TIMEOUT.as_secs()
        )
    })
}

/// A label for the hub's sources table derived from the capture source.
async fn default_label(source: &CaptureTarget) -> String {
    match source {
        CaptureTarget::SystemMix | CaptureTarget::SystemMixExcludingSelf => {
            "System audio".to_owned()
        }
        CaptureTarget::Process { pid } => {
            let pid = *pid;
            let name = blocking(move || {
                hfa_capture::list_capture_apps()
                    .ok()
                    .and_then(|apps| apps.into_iter().find(|a| a.pid == pid))
                    .map(|a| a.name)
            })
            .await
            .ok()
            .flatten();
            name.unwrap_or_else(|| format!("Process {pid}"))
        }
        CaptureTarget::Tone { freq_hz } => format!("Tone {freq_hz} Hz"),
        CaptureTarget::WavFile(path) => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "WAV file".to_owned()),
        CaptureTarget::External { id } => format!("External {id}"),
    }
}

/// Lower-case state name.
fn state_name(state: &SenderState) -> &'static str {
    match state {
        SenderState::Connecting => "connecting",
        SenderState::Pairing => "pairing",
        SenderState::Streaming => "streaming",
        SenderState::Reconnecting => "reconnecting",
        SenderState::Stopped => "stopped",
        SenderState::Failed(_) => "failed",
    }
}

/// One line for a sender event (`None` for events shown otherwise).
fn describe_event(ev: &SenderEvent) -> Option<String> {
    Some(match ev {
        SenderEvent::StateChanged(_) | SenderEvent::Status(_) => return None,
        SenderEvent::Connected { device_id, name } => {
            format!("connected to hub {name:?} ({device_id})")
        }
        SenderEvent::Paired { device_id, name } => {
            format!("paired with hub {name:?} ({device_id}); no PIN needed next time")
        }
        SenderEvent::HubControl {
            gain,
            muted,
            priority,
        } => format!(
            "hub settings for this stream: gain {gain:.2}{}{}",
            if *muted { ", muted" } else { "" },
            if *priority { ", priority" } else { "" }
        ),
        SenderEvent::Error(message) => format!("warning: {message}"),
    })
}

/// The periodic status line.
fn status_line(sender: &SenderHandle) -> String {
    let s = sender.status();
    let (audio, keepalives) = sender.packets_sent();
    format!(
        "[{}] {} kbit/s  loss {}  rtt {}  level {}  sent {audio} packets + {keepalives} keep-alives",
        state_name(&s.state),
        s.bitrate / 1000,
        percent(s.loss_pct),
        millis(s.rtt_ms),
        level(s.level_db),
    )
}

/// Turns a final sender failure (a [`CoreError`] message) into an actionable error.
fn explain_failure(message: &str) -> anyhow::Error {
    let prefix = |e: CoreError| {
        let text = e.to_string();
        text.trim_end_matches(char::is_alphanumeric).to_owned()
    };
    if message == CoreError::PairingRequired.to_string() {
        anyhow!(
            "pairing required: this device and the hub have not paired yet. Open a pairing \
             window on the hub (`hfa hub --pair`, or Pair in the app) and pass the PIN it shows \
             (--pin 123456) or its pairing URI (--uri 'hfa://pair?...')."
        )
    } else if message.starts_with(&prefix(CoreError::PairingFailed("x".into()))) {
        anyhow!(
            "{message}. Check the PIN or URI: a pairing window works for one device and closes \
             after 5 minutes or 5 wrong attempts, so open a new one on the hub and try again."
        )
    } else if message.starts_with(&prefix(CoreError::KeyMismatch("x".into()))) {
        anyhow!(
            "{message}: the device answering is not the hub you asked for (another hub, or \
             someone impersonating it). If the hub was reinstalled, forget it with \
             `hfa trust remove <id>` and pair again."
        )
    } else if message.starts_with(&prefix(CoreError::HubNotFound("x".into()))) {
        anyhow!("{message}. Use `hfa discover` to list hubs, or --to <ip>.")
    } else {
        anyhow!("the sender gave up: {message}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Cli;
    use clap::Parser;

    fn send_args(argv: &[&str]) -> SendArgs {
        match Cli::try_parse_from(argv).expect("valid").command {
            crate::cli::Command::Send(a) => a,
            other => panic!("unexpected {other:?}"),
        }
    }

    fn empty_trust() -> (tempfile::TempDir, TrustStore) {
        let dir = tempfile::tempdir().unwrap();
        let trust = TrustStore::load(dir.path()).unwrap();
        (dir, trust)
    }

    #[tokio::test]
    async fn uri_supplies_address_key_and_token() {
        let key = [7u8; 32];
        let uri = hfa_proto::PairingUri::new("127.0.0.1", 5000, key, "tok_en-123", "Desk")
            .unwrap()
            .to_string();
        let (_dir, trust) = empty_trust();
        let t = resolve_target(&send_args(&["hfa", "send", "--uri", &uri]), &trust)
            .await
            .unwrap();
        assert_eq!(
            t.hub,
            HubAddress::Direct {
                host: "127.0.0.1".into(),
                port: 5000
            }
        );
        assert_eq!(t.expected_hub_key, Some(key));
        assert_eq!(t.secret.as_deref(), Some("tok_en-123"));

        // --to overrides host and port but keeps the key and token.
        let t = resolve_target(
            &send_args(&["hfa", "send", "--uri", &uri, "--to", "localhost:6000"]),
            &trust,
        )
        .await
        .unwrap();
        assert_eq!(
            t.hub,
            HubAddress::Direct {
                host: "localhost".into(),
                port: 6000
            }
        );
        assert_eq!(t.expected_hub_key, Some(key));
        assert_eq!(t.secret.as_deref(), Some("tok_en-123"));
    }

    #[tokio::test]
    async fn pin_is_the_secret_and_unknown_hosts_fail_fast() {
        let (_dir, trust) = empty_trust();
        let t = resolve_target(
            &send_args(&["hfa", "send", "--to", "127.0.0.1", "--pin", "012345"]),
            &trust,
        )
        .await
        .unwrap();
        assert_eq!(t.secret.as_deref(), Some("012345"));
        assert_eq!(t.expected_hub_key, None);
        assert_eq!(t.shown, "127.0.0.1:47810");

        let err = resolve_target(
            &send_args(&["hfa", "send", "--to", "no-such-host.invalid"]),
            &trust,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().starts_with("hub not found"), "{err}");
    }

    #[test]
    fn failures_are_explained() {
        let e = explain_failure(&CoreError::PairingRequired.to_string()).to_string();
        assert!(e.contains("--pin") && e.contains("--uri"), "{e}");
        let e = explain_failure(&CoreError::PairingFailed("wrong PIN or token".into()).to_string())
            .to_string();
        assert!(e.contains("wrong PIN") && e.contains("new one"), "{e}");
        let e = explain_failure(&CoreError::KeyMismatch("ab12-cd34-ef56-7890".into()).to_string())
            .to_string();
        assert!(
            e.contains("ab12-cd34-ef56-7890") && e.contains("hfa trust remove"),
            "{e}"
        );
        let e = explain_failure(&CoreError::HubNotFound("Desk".into()).to_string()).to_string();
        assert!(e.contains("hfa discover"), "{e}");
        let e = explain_failure("closed").to_string();
        assert!(e.contains("gave up: closed"), "{e}");
    }

    #[tokio::test]
    async fn labels_follow_the_source() {
        assert_eq!(
            default_label(&CaptureTarget::SystemMix).await,
            "System audio"
        );
        assert_eq!(
            default_label(&CaptureTarget::Tone { freq_hz: 440.0 }).await,
            "Tone 440 Hz"
        );
        assert_eq!(
            default_label(&CaptureTarget::WavFile("/music/song.wav".into())).await,
            "song.wav"
        );
    }

    #[test]
    fn describes_events() {
        let line = describe_event(&SenderEvent::HubControl {
            gain: 0.5,
            muted: true,
            priority: false,
        })
        .unwrap();
        assert_eq!(line, "hub settings for this stream: gain 0.50, muted");
        assert!(describe_event(&SenderEvent::StateChanged(SenderState::Streaming)).is_none());
    }
}
