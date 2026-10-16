use std::path::PathBuf;

#[derive(Debug, clap::Args)]
#[command(group(clap::ArgGroup::new("workspace_selection").args(["workspace", "workspace_id", "no_workspace"]).multiple(false)))]
pub struct ChatArgs {
    #[arg(long)]
    pub session: Option<String>,
    #[arg(long)]
    pub model: Option<String>,
    /// Open this existing directory before creating a new workspace-backed session.
    #[arg(long, conflicts_with = "session")]
    pub workspace: Option<PathBuf>,
    /// Use a workspace already registered with the daemon.
    #[arg(long, conflicts_with = "session")]
    pub workspace_id: Option<String>,
    /// Select the built-in directory-less workspace explicitly.
    #[arg(long, conflicts_with = "session")]
    pub no_workspace: bool,
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
    #[arg(long, value_parser=["after-turn", "after-finish", "next-boundary", "immediate"], requires="session")]
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
