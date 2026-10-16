use std::path::PathBuf;

use clap::Parser;
use slop_protocol::DEFAULT_DAEMON_URL;

use super::Command;

#[derive(Debug, Parser)]
#[command(name = "slop", version, about = "Hive command-line client")]
pub struct Args {
    /// Daemon origin to connect to.
    #[arg(long, global = true, env = "SLOP_DAEMON_URL", default_value = DEFAULT_DAEMON_URL)]
    pub daemon: String,

    /// Emit machine-readable JSON on standard output.
    #[arg(long, global = true)]
    pub json: bool,

    /// Read the local API bearer token from this file.
    #[arg(long, global = true)]
    pub token_file: Option<PathBuf>,

    /// Select an explicitly registered owner through the local daemon.
    #[arg(long, global = true)]
    pub peer: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}
