use clap::Subcommand;
use std::path::PathBuf;

#[derive(Debug, Subcommand)]
pub enum TaskCommand {
    /// Submit a durable job and return its IDs without waiting.
    Create {
        #[arg(long)]
        model: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long, conflicts_with = "prompt_file")]
        text: Option<String>,
        #[arg(long)]
        prompt_file: Option<PathBuf>,
        #[arg(long)]
        effort: Option<String>,
        #[arg(long)]
        max_output_tokens: Option<u32>,
        #[arg(long)]
        max_model_requests: Option<u32>,
        #[arg(long)]
        max_tool_calls: Option<u32>,
        /// JSON policy explicitly authorizing bounded child creation.
        #[arg(long)]
        orchestration_policy: Option<PathBuf>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long, requires = "project")]
        base: Option<String>,
        #[arg(long="tool",requires="project",value_parser=["read","write","edit","bash"])]
        tools: Vec<String>,
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
    Runs {
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
    /// Follow the attempt current when this command attaches.
    Follow {
        id: String,
        #[arg(long, default_value_t = 0)]
        after: u64,
    },
    /// Steer the current active or paused attempt.
    Send {
        id: String,
        #[arg(long, conflicts_with = "prompt_file")]
        text: Option<String>,
        #[arg(long)]
        prompt_file: Option<PathBuf>,
        #[arg(long,value_parser=["next-boundary","immediate"],default_value="next-boundary")]
        delivery: String,
        #[arg(long)]
        command_id: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum RunCommand {
    CreateChild {
        id: String,
        file: PathBuf,
        #[arg(long)]
        command_id: Option<String>,
    },
    Children {
        id: String,
        #[arg(long)]
        after: Option<u64>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    Wait {
        id: String,
        #[arg(long = "child-run", required = true)]
        child_run_ids: Vec<String>,
        #[arg(long)]
        command_id: Option<String>,
    },
    Result {
        id: String,
    },
    Show {
        id: String,
    },
    Pause {
        id: String,
        #[arg(long)]
        command_id: Option<String>,
    },
    Resume {
        id: String,
        #[arg(long)]
        command_id: Option<String>,
    },
    Cancel {
        id: String,
        #[arg(long)]
        command_id: Option<String>,
    },
    /// Start a fresh attempt, retaining the previous session/workspace.
    Retry {
        id: String,
        #[arg(long)]
        command_id: Option<String>,
        #[arg(long)]
        acknowledge_unknown_effects: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum BatchCommand {
    /// Cancel all unfinished members and close the batch to retries.
    Cancel {
        id: String,
        #[arg(long)]
        command_id: Option<String>,
    },
    /// Validate and expand without creating jobs; optionally save frozen inputs.
    Preview {
        file: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
    },
    Submit {
        file: PathBuf,
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
    Members {
        id: String,
        #[arg(long)]
        after: Option<u64>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    Results {
        id: String,
        #[arg(long)]
        after: Option<u64>,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Export every member's latest outcome as JSON Lines.
    Export {
        id: String,
        #[arg(long)]
        output: PathBuf,
    },
    /// Retry only selected unsuccessful combination indexes.
    Retry {
        id: String,
        #[arg(long = "index", required = true)]
        indices: Vec<u32>,
        #[arg(long)]
        command_id: Option<String>,
        #[arg(long)]
        acknowledge_unknown_effects: bool,
    },
}
