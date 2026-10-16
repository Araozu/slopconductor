use std::path::PathBuf;

use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum ArtifactCommand {
    Show {
        id: String,
    },
    Download {
        id: String,
        #[arg(long)]
        output: PathBuf,
    },
}
