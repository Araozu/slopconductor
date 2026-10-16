use std::{net::SocketAddr, path::PathBuf};

use clap::Args;

#[derive(Debug, Args)]
pub struct StartupArgs {
    /// Local listen address. Only loopback listeners are supported.
    #[arg(long, env = "SLOP_LISTEN")]
    pub listen: Option<SocketAddr>,
    /// Optional loopback peer API listener, for private HTTPS forwarding.
    #[arg(long, env = "SLOP_PEER_LISTEN")]
    pub peer_listen: Option<SocketAddr>,
    /// Configuration file (defaults to the platform config directory).
    #[arg(long, env = "SLOP_CONFIG")]
    pub config: Option<PathBuf>,
    /// Persistent application data directory.
    #[arg(long, env = "SLOP_DATA_DIR")]
    pub data_dir: Option<PathBuf>,
    /// Explicit trusted OpenCode Go-compatible endpoint, mainly for gateways
    /// operated by the user and controlled local fixtures.
    #[arg(long, env = "SLOP_PROVIDER_BASE_URL")]
    pub provider_base_url: Option<String>,
    #[arg(long, env = "SLOP_OPENCODE_ZEN_BASE_URL")]
    pub opencode_zen_base_url: Option<String>,
    #[arg(long, env = "SLOP_CODEX_BASE_URL")]
    pub codex_base_url: Option<String>,
    #[arg(long, env = "SLOP_DEFAULT_MODEL")]
    pub default_model: Option<String>,
}
