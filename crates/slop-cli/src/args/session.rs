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
    /// Create a new conversation with history through the selected message.
    Fork {
        id: String,
        #[arg(long)]
        message: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        command_id: Option<String>,
        #[arg(long)]
        expected_revision: Option<u64>,
    },
    /// Edit a message in a new fork; user edits start a new response.
    Edit {
        id: String,
        #[arg(long)]
        message: String,
        #[arg(long, conflicts_with = "text")]
        prompt_file: Option<PathBuf>,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        command_id: Option<String>,
        #[arg(long)]
        expected_revision: Option<u64>,
        #[arg(long)]
        detach: bool,
    },
    /// Delete the selected message and everything after it in an idle session.
    DeleteMessage {
        id: String,
        #[arg(long)]
        message: String,
        #[arg(long)]
        command_id: Option<String>,
        #[arg(long)]
        expected_revision: Option<u64>,
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
        /// Opt in to the model's full window; =false restores the local cost cap.
        #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "true")]
        full_context: Option<bool>,
        #[arg(long, value_parser=["after-turn", "next-boundary", "immediate"])]
        delivery: Option<String>,
    },
    Follow {
        id: String,
        #[arg(long, default_value_t = 0)]
        after: u64,
    },
}
