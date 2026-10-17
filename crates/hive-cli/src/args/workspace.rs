use clap::Subcommand;
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub enum WorkspaceCommand {
    /// Open an existing directory as a workspace.
    Open {
        path: PathBuf,
        #[arg(long)]
        command_id: Option<String>,
    },
    /// Create a new folder workspace, optionally initializing Git.
    Create {
        parent: PathBuf,
        name: String,
        #[arg(long)]
        git: bool,
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
    /// Create a managed Git worktree as a new workspace.
    Worktree {
        id: String,
        #[arg(long)]
        base: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        command_id: Option<String>,
    },
    Diff {
        id: String,
    },
    Events {
        id: String,
        #[arg(long)]
        after: Option<u64>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    Rename {
        id: String,
        name: String,
        #[arg(long)]
        command_id: Option<String>,
    },
    /// Set an opaque RGB color (#RRGGBB, no alpha).
    Color {
        id: String,
        color: String,
        #[arg(long)]
        command_id: Option<String>,
    },
    Remove {
        id: String,
        #[arg(long)]
        command_id: Option<String>,
    },
}
