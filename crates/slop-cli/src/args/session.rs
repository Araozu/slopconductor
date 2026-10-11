use std::path::PathBuf;

use clap::Subcommand;

#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    List {
        #[arg(long)]
        after: Option<u64>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    Show {
        id: String,
    },
    History {
        id: String,
        #[arg(long)]
        after: Option<u64>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    Send {
        id: String,
        #[arg(long, conflicts_with = "text")]
        prompt_file: Option<PathBuf>,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        command_id: Option<String>,
        #[arg(long)]
        detach: bool,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        effort: Option<String>,
        #[arg(long)]
        max_output_tokens: Option<u32>,
        #[arg(long, value_parser=["after-turn", "next-boundary", "immediate"])]
        delivery: Option<String>,
    },
    Follow {
        id: String,
        #[arg(long, default_value_t = 0)]
        after: u64,
    },
}
