//! Shared helpers and the small commands: `hfa discover`, `hfa devices`, `hfa trust`.
//! The larger commands live in `hub.rs`, `send.rs` and `selftest.rs`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context};
use hfa_core::{DiscoveryEvent, HubInfo, TrustStore};

use crate::cli::{DiscoverArgs, TrustCommand};
use crate::display::{format_unix_time, Table};

/// The data directory: `--data-dir`, else the platform default
/// ([`hfa_core::config::default_data_dir`]).
pub fn data_dir(arg: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    arg.or_else(hfa_core::config::default_data_dir)
        .ok_or_else(|| anyhow!("no home directory is known on this system; pass --data-dir <DIR>"))
}

/// Runs a blocking closure (disk or OS audio API calls) off the async workers.
pub async fn blocking<T, F>(f: F) -> anyhow::Result<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| anyhow!("background task failed: {e}"))
}

/// Loads the trust store of `dir` (off the async workers).
pub async fn load_trust(dir: PathBuf) -> anyhow::Result<TrustStore> {
    let shown = dir.display().to_string();
    blocking(move || TrustStore::load(&dir))
        .await?
        .with_context(|| format!("cannot read the trusted devices in {shown}"))
}

/// Human-readable address list of a discovered hub (`ip:port, ...`).
pub fn hub_addrs(info: &HubInfo) -> String {
    if info.addrs.is_empty() {
        return "-".to_owned();
    }
    info.addrs
        .iter()
        .map(|ip| std::net::SocketAddr::new(*ip, info.port).to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// `hfa discover`: browses mDNS for `--timeout` seconds, printing hubs as they appear and
/// disappear, then a summary table (trusted hubs are marked).
pub async fn discover(data_dir: Option<PathBuf>, args: DiscoverArgs) -> anyhow::Result<()> {
    // The trust store is optional here: discovery works without a data dir.
    let trust = match data_dir.or_else(hfa_core::config::default_data_dir) {
        Some(dir) if dir.join(hfa_core::identity::TRUST_FILE).exists() => {
            load_trust(dir).await.ok()
        }
        _ => None,
    };
    let is_trusted = |id: &str| trust.as_ref().is_some_and(|t| t.get(id).is_some());
    let mut browser = hfa_core::browse().context("cannot start mDNS browsing")?;
    println!(
        "Browsing for hubs ({}) for {} s...",
        hfa_proto::SERVICE_TYPE,
        args.timeout
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(args.timeout);
    let mut hubs: BTreeMap<String, HubInfo> = BTreeMap::new();
    loop {
        tokio::select! {
            ev = tokio::time::timeout_at(deadline, browser.recv()) => match ev {
                Err(_) | Ok(None) => break,
                Ok(Some(DiscoveryEvent::Found(info))) => {
                    if !hubs.contains_key(&info.device_id) {
                        println!(
                            "  found {:?} ({}) at {}{}",
                            info.name,
                            info.device_id,
                            hub_addrs(&info),
                            if is_trusted(&info.device_id) { " [trusted]" } else { "" }
                        );
                    }
                    hubs.insert(info.device_id.clone(), info);
                }
                Ok(Some(DiscoveryEvent::Lost(id))) => {
                    if hubs.remove(&id).is_some() {
                        println!("  lost {id}");
                    }
                }
            },
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    drop(browser);
    println!();
    if hubs.is_empty() {
        println!("No hub found. Is a hub running on this network (`hfa hub`)?");
        println!("Multicast DNS may be blocked (firewall, guest Wi-Fi); use `hfa send --to <ip>`.");
        return Ok(());
    }
    let mut table = Table::new(["NAME", "DEVICE ID", "ADDRESS", "PLATFORM", "TRUSTED"]);
    for info in hubs.values() {
        table.row([
            info.name.clone(),
            info.device_id.clone(),
            hub_addrs(info),
            info.platform.clone(),
            if is_trusted(&info.device_id) {
                "yes"
            } else {
                "no"
            }
            .to_owned(),
        ]);
    }
    print!("{}", table.render());
    Ok(())
}

/// `hfa devices`: output devices, capture capabilities and capturable apps.
pub async fn devices() -> anyhow::Result<()> {
    let (outputs, caps, apps) = blocking(|| {
        (
            hfa_capture::list_output_devices(),
            hfa_capture::capabilities(),
            hfa_capture::list_capture_apps(),
        )
    })
    .await?;

    println!("Output devices (hfa hub --out device:<name>):");
    match outputs {
        Ok(list) if list.is_empty() => println!("  (none found)"),
        Ok(list) => list.iter().for_each(|d| println!("  {d}")),
        Err(e) => println!("  unavailable: {e}"),
    }
    println!("  also: default, null, wav:<path>");

    println!();
    println!("Capture capabilities ({}):", hfa_core::platform_name());
    let yes_no = |b: bool| if b { "yes" } else { "no" };
    println!(
        "  system audio (--source system | system-excl): {}",
        yes_no(caps.system_mix)
    );
    println!(
        "  per-app capture (--source pid:<n>):            {}",
        yes_no(caps.per_app)
    );
    println!(
        "  capturing mutes the local speakers:            {}",
        yes_no(caps.mutes_local_output)
    );
    if !caps.notes.trim().is_empty() {
        println!("  notes: {}", caps.notes.trim());
    }
    println!("  always available: tone:<hz>, wav:<path>");

    println!();
    println!("Apps playing audio (--source pid:<n>):");
    match apps {
        Ok(list) if list.is_empty() => println!("  (none right now)"),
        Ok(list) => {
            let mut table = Table::new(["PID", "NAME"]);
            for app in list {
                table.row([app.pid.to_string(), app.name]);
            }
            for line in table.render().lines() {
                println!("  {line}");
            }
        }
        Err(e) => println!("  unavailable: {e}"),
    }
    Ok(())
}

/// `hfa trust list|remove <id>`.
pub async fn trust(data_dir: PathBuf, command: TrustCommand) -> anyhow::Result<()> {
    let store = load_trust(data_dir.clone()).await?;
    match command {
        TrustCommand::List => {
            let peers = store.peers();
            if peers.is_empty() {
                println!("No trusted devices in {}.", data_dir.display());
                return Ok(());
            }
            let mut table = Table::new(["DEVICE ID", "NAME", "PAIRED AT (UTC)"]);
            for peer in peers {
                table.row([peer.device_id, peer.name, format_unix_time(peer.paired_at)]);
            }
            print!("{}", table.render());
        }
        TrustCommand::Remove { device_id } => {
            let id = device_id.trim().to_owned();
            let name = store.get(&id).map(|p| p.name);
            let removed = blocking(move || store.remove(&id))
                .await?
                .context("cannot update the trusted devices")?;
            if !removed {
                return Err(anyhow!(
                    "{:?} is not a trusted device (see `hfa trust list`)",
                    device_id.trim()
                ));
            }
            println!(
                "Removed {} ({}). It must pair again before it can connect.",
                device_id.trim(),
                name.unwrap_or_default()
            );
        }
    }
    Ok(())
}
