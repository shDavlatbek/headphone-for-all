//! Command implementations (owned by `feat/cli`).

use std::path::PathBuf;

use anyhow::bail;

use crate::cli::{DiscoverArgs, HubArgs, SelftestArgs, SendArgs, TrustCommand};

/// `hfa hub`: start a hub, optionally print the PIN/URI, show a live sources table.
pub async fn hub(_data_dir: Option<PathBuf>, _args: HubArgs) -> anyhow::Result<()> {
    bail!("not implemented yet")
}

/// `hfa send`: capture and stream to a hub.
pub async fn send(_data_dir: Option<PathBuf>, _args: SendArgs) -> anyhow::Result<()> {
    bail!("not implemented yet")
}

/// `hfa discover`: browse mDNS for hubs.
pub async fn discover(_args: DiscoverArgs) -> anyhow::Result<()> {
    bail!("not implemented yet")
}

/// `hfa devices`: outputs, capturable apps, capabilities.
pub async fn devices() -> anyhow::Result<()> {
    bail!("not implemented yet")
}

/// `hfa trust list|remove`.
pub async fn trust(_data_dir: Option<PathBuf>, _command: TrustCommand) -> anyhow::Result<()> {
    bail!("not implemented yet")
}

/// `hfa selftest`: in-process hub + tone sender over localhost; non-zero exit on failure.
pub async fn selftest(_args: SelftestArgs) -> anyhow::Result<()> {
    bail!("not implemented yet")
}
