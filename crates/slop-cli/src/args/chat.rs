use std::path::PathBuf;

#[derive(Debug, clap::Args)]
#[command(group(clap::ArgGroup::new("workspace_selection").args(["workspace", "project"]).multiple(false)))]
pub struct ChatArgs {
    #[arg(long)]
    pub session: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    /// Explicit workspace on the daemon's machine for a new tool-enabled session.
    #[arg(long, conflicts_with = "session")]
    pub workspace: Option<PathBuf>,
    /// Registered project on the daemon; reserve an isolated managed worktree.
    #[arg(long, conflicts_with = "session")]
    pub project: Option<String>,
    /// Git revision to freeze when reserving a project worktree (default HEAD).
    #[arg(long, requires = "project")]
    pub base: Option<String>,
    /// Tool to allow; repeat to restrict the default read/write/edit/bash set.
    #[arg(long="tool", requires="workspace_selection", value_parser=["read","write","edit","bash"])]
    pub tools: Vec<String>,
    #[arg(long)]
    pub effort: Option<String>,
    #[arg(long)]
    pub max_output_tokens: Option<u32>,
    /// Opt in to the model's full window; =false restores the local cost cap.
    #[arg(long, num_args = 0..=1, require_equals = true, default_missing_value = "true")]
    pub full_context: Option<bool>,
    #[arg(long, value_parser=["after-turn", "next-boundary", "immediate"], requires="session")]
    pub delivery: Option<String>,
    #[arg(long, conflicts_with = "prompt_file")]
    pub prompt: Option<String>,
    #[arg(long)]
    pub prompt_file: Option<PathBuf>,
    #[arg(long)]
    pub command_id: Option<String>,
    /// Return after durable acceptance without following the turn.
    #[arg(long)]
    pub detach: bool,
}
