use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum WorkspaceCommand {
    Show {
        id: String,
    },
    Diff {
        id: String,
    },
    Remove {
        id: String,
        #[arg(long)]
        command_id: Option<String>,
    },
}
