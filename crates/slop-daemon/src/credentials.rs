//! Daemon-owned provider secrets, separate from conversation data and events.

use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

use slop_protocol::providers::{
    LoginResponse, LoginStatus, MAX_API_KEY_BYTES, ProviderAccount, ProviderStatus,
    SelectAuthRequest, SignOutResponse,
};
use slop_runtime::{
    chat::ChatRuntime,
    providers::{
        ProviderClient,
        chatgpt_auth::{ChatGptConnection, ChatGptLogin},
        codex::CodexClient,
        opencode_go::OpencodeGoClient,
        opencode_zen::OpencodeZenClient,
    },
};
use tokio::sync::{Mutex, watch};

use crate::auth::{create_private_dir, open_private_file, write_private_file};

pub const PROVIDERS: &[(&str, &str)] = &[
    ("opencode-go", "OPENCODE_GO_API_KEY"),
    ("opencode-zen", "OPENCODE_ZEN_API_KEY"),
    ("codex", "OPENAI_API_KEY"),
];

#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    #[error("provider is not supported")]
    Unsupported,
    #[error("API key must be nonempty, printable ASCII without whitespace, and at most 16 KiB")]
    InvalidKey,
    #[error("provider credential storage is unavailable or is not private")]
    Storage,
    #[error("a ChatGPT login is already pending")]
    LoginPending,
    #[error("ChatGPT login could not be started")]
    LoginFailed,
    #[error("login attempt was not found")]
    NotFound,
    #[error("the daemon is shutting down")]
    Stopping,
    #[error("invalid login command identifier")]
    InvalidId,
    #[error("the selected authentication mode or account is unavailable")]
    Unavailable,
}

pub struct ProviderCredentials {
    directory: PathBuf,
    host_id: String,
    zen_base_url: Option<String>,
    codex_base_url: Option<String>,
    state: Mutex<CredentialState>,
    shutdown: watch::Sender<bool>,
}

struct CredentialState {
    api_keys: HashMap<String, String>,
    chatgpt: Option<Arc<ChatGptConnection>>,
    accounts: HashMap<String, Arc<ChatGptConnection>>,
    active_account: Option<String>,
    active_codex_auth: Option<String>,
    login: Option<LoginRecord>,
    stopping: bool,
}

struct LoginRecord {
    account_id: String,
    response: LoginResponse,
    status: LoginStatus,
    task: Option<tokio::task::JoinHandle<()>>,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CodexSelection {
    auth_mode: Option<String>,
    account_id: Option<String>,
}

fn write_codex_selection(
    path: &Path,
    mode: Option<&str>,
    account: Option<&str>,
) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(&CodexSelection {
        auth_mode: mode.map(str::to_owned),
        account_id: account.map(str::to_owned),
    })
    .map_err(std::io::Error::other)?;
    // Mode and account are one atomic replacement: an unsuccessful switch
    // must not select a different subscription registration after restart.
    write_private_file(path, &bytes).map_err(std::io::Error::other)
}

fn valid_account_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

fn account_path(directory: &Path, id: &str) -> PathBuf {
    if id == "default" {
        directory.join("codex-chatgpt.json")
    } else {
        directory.join(format!("codex-chatgpt-{id}.json"))
    }
}

fn key_path(directory: &Path, provider: &str) -> PathBuf {
    directory.join(format!("{provider}-api-key"))
}

fn check_provider(provider: &str) -> Result<(), CredentialError> {
    if PROVIDERS.iter().any(|(id, _)| *id == provider) {
        Ok(())
    } else {
        Err(CredentialError::Unsupported)
    }
}

fn validate_key(key: &str) -> Result<&str, CredentialError> {
    let key = key.trim();
    if key.is_empty()
        || key.len() > MAX_API_KEY_BYTES
        || !key.bytes().all(|byte| (33..=126).contains(&byte))
    {
        Err(CredentialError::InvalidKey)
    } else {
        Ok(key)
    }
}

fn status(state: &CredentialState, provider: &str) -> ProviderStatus {
    ProviderStatus {
        provider: provider.to_owned(),
        api_key_configured: state.api_keys.contains_key(provider),
        chatgpt_configured: provider == "codex" && state.chatgpt.is_some(),
        execution_supported: true,
        active_auth_mode: (provider == "codex")
            .then(|| state.active_codex_auth.clone())
            .flatten(),
    }
}

impl ProviderCredentials {}

mod auth;
#[cfg(test)]
#[path = "credential_tests.rs"]
mod credential_tests;
mod loading;
mod login;
mod providers;
