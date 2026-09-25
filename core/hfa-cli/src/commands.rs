//! Shared helpers and the small commands: `hfa discover`, `hfa devices`, `hfa trust`.
//! The larger commands live in `hub.rs`, `send.rs` and `selftest.rs`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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

/// Lock file in the data dir: `hfa hub` and `hfa send` hold a shared lock on it while they
/// run, `hfa trust remove` takes an exclusive one (see [`DataDirLock`]).
pub const LOCK_FILE: &str = "hfa.lock";

/// An advisory lock on [`LOCK_FILE`] of a data dir, released on drop (and by the OS when the
/// process ends, even if it crashes).
///
/// A running hub or sender keeps its own copy of the trusted devices in memory: a device
/// removed from `trusted.json` behind its back would still be accepted, and the next
/// pairing would write it back. So `hfa hub` / `hfa send` hold a **shared** lock for their
/// whole run and `hfa trust remove` refuses to change the store unless it gets the
/// **exclusive** lock.
#[derive(Debug)]
pub struct DataDirLock {
    _file: std::fs::File,
}

impl DataDirLock {
    fn open(dir: &Path) -> std::io::Result<std::fs::File> {
        std::fs::create_dir_all(dir)?;
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(LOCK_FILE))
    }

    /// Takes the shared lock of a running hub / sender (waits while a `trust remove` holds
    /// the exclusive lock, which takes milliseconds). Creates `dir` if needed.
    pub async fn shared(dir: PathBuf) -> anyhow::Result<Self> {
        let shown = dir.display().to_string();
        blocking(move || -> std::io::Result<Self> {
            let file = Self::open(&dir)?;
            // Fully qualified: std's inherent `File::lock_shared` (Rust 1.89) is above the MSRV.
            fs4::FileExt::lock_shared(&file)?;
            Ok(Self { _file: file })
        })
        .await?
        .with_context(|| format!("cannot lock the data dir {shown}"))
    }

    /// Tries to take the exclusive lock: `Ok(None)` while a hub or sender runs with `dir`.
    pub fn try_exclusive(dir: &Path) -> anyhow::Result<Option<Self>> {
        let file = Self::open(dir)
            .with_context(|| format!("cannot open the lock file in {}", dir.display()))?;
        match fs4::FileExt::try_lock(&file) {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(fs4::TryLockError::WouldBlock) => Ok(None),
            Err(fs4::TryLockError::Error(e)) => {
                Err(e).with_context(|| format!("cannot lock the data dir {}", dir.display()))
            }
        }
    }
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

/// `hfa trust list|remove <id>`. `remove` refuses while a hub or sender runs with the same
/// data dir (see [`DataDirLock`]).
pub async fn trust(data_dir: PathBuf, command: TrustCommand) -> anyhow::Result<()> {
    // Held until the removal is saved, so no hub or sender starts with the old list meanwhile.
    let _lock = match command {
        TrustCommand::Remove { .. } if data_dir.exists() => {
            let dir = data_dir.clone();
            match blocking(move || DataDirLock::try_exclusive(&dir)).await?? {
                Some(lock) => Some(lock),
                None => {
                    return Err(anyhow!(
                        "a running `hfa hub` or `hfa send` uses {}; stop it first. It keeps its \
                         own copy of the trusted devices, so it would still accept the device \
                         (and its next pairing would save it again)",
                        data_dir.display()
                    ))
                }
            }
        }
        _ => None,
    };
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
