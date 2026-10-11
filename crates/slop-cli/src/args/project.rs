use std::path::PathBuf;

use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    Register {
        /// Absolute repository checkout path on the daemon's machine.
        path: PathBuf,
        #[arg(long)]
        command_id: Option<String>,
    },
    List {
        #[arg(long)]
        after: Option<u64>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    Show {
        id: String,
    },
    Workspaces {
        id: String,
        #[arg(long)]
        after: Option<u64>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    Events {
        id: String,
        #[arg(long)]
        after: Option<u64>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
}
