use std::net::SocketAddr;

use serde::Deserialize;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FileConfig {
    pub(super) listen: Option<SocketAddr>,
    pub(super) peer_listen: Option<SocketAddr>,
    pub(super) node_name: Option<String>,
    pub(super) database_queue_capacity: Option<usize>,
    pub(super) database_busy_timeout_ms: Option<u64>,
    pub(super) shutdown_timeout_ms: Option<u64>,
    pub(super) provider_base_url: Option<String>,
    pub(super) opencode_zen_base_url: Option<String>,
    pub(super) codex_base_url: Option<String>,
    pub(super) default_model: Option<String>,
    pub(super) max_output_tokens: Option<u32>,
    pub(super) context_token_budget: Option<u64>,
    pub(super) execution_concurrency: Option<usize>,
}
