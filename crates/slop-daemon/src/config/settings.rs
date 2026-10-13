use std::{net::SocketAddr, path::PathBuf, time::Duration};

#[derive(Clone, Debug)]
pub struct Config {
    pub listen: SocketAddr,
    pub node_name: Option<String>,
    pub database_queue_capacity: usize,
    pub database_busy_timeout: Duration,
    pub shutdown_timeout: Duration,
    pub data_dir: PathBuf,
    pub provider_base_url: Option<String>,
    pub opencode_zen_base_url: Option<String>,
    pub codex_base_url: Option<String>,
    pub default_model: String,
    pub max_output_tokens: u32,
    pub context_token_budget: u64,
    pub execution_concurrency: usize,
}
