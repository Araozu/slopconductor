use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum WorkspaceCommand {
    Show {
        id: String,
    },
    Diff {
        id: String,
    },
    Rename {
        id: String,
        name: String,
        #[arg(long)]
        command_id: Option<String>,
    },
    Remove {
        id: String,
        #[arg(long)]
        command_id: Option<String>,
    },
}
