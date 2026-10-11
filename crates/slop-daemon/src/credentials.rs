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

impl ProviderCredentials {
    /// Restore private records. Legacy environment keys are imported only when
    /// a stored key does not exist; saved runtime changes win on every restart.
    #[cfg(test)]
    pub async fn load(
        data_dir: &Path,
        host_id: String,
        bootstrap: Vec<(String, String)>,
    ) -> Result<(Arc<Self>, Option<String>), CredentialError> {
        Self::load_with_endpoints(data_dir, host_id, bootstrap, None, None).await
    }

    pub async fn load_with_endpoints(
        data_dir: &Path,
        host_id: String,
        bootstrap: Vec<(String, String)>,
        zen_base_url: Option<String>,
        codex_base_url: Option<String>,
    ) -> Result<(Arc<Self>, Option<String>), CredentialError> {
        let directory = data_dir.join("credentials");
        let read_directory = directory.clone();
        let (api_keys, account_ids) = tokio::task::spawn_blocking(move || {
            create_private_dir(&read_directory).map_err(|_| CredentialError::Storage)?;
            let mut api_keys = HashMap::new();
            for (provider, _) in PROVIDERS {
                let path = key_path(&read_directory, provider);
                let key = match std::fs::symlink_metadata(&path) {
                    Ok(_) => {
                        let mut bytes = Vec::new();
                        open_private_file(&path)
                            .map_err(|_| CredentialError::Storage)?
                            .take((MAX_API_KEY_BYTES + 1) as u64)
                            .read_to_end(&mut bytes)
                            .map_err(|_| CredentialError::Storage)?;
                        if bytes.len() > MAX_API_KEY_BYTES {
                            return Err(CredentialError::Storage);
                        }
                        let key = String::from_utf8(bytes).map_err(|_| CredentialError::Storage)?;
                        Some(
                            validate_key(&key)
                                .map_err(|_| CredentialError::Storage)?
                                .to_owned(),
                        )
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        let disabled = read_directory.join(format!("{provider}-api-key-disabled"));
                        let disabled = match std::fs::symlink_metadata(&disabled) {
                            Ok(_) => {
                                open_private_file(&disabled)
                                    .map_err(|_| CredentialError::Storage)?;
                                true
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                            Err(_) => return Err(CredentialError::Storage),
                        };
                        if let Some((_, key)) = bootstrap
                            .iter()
                            .find(|(id, _)| id == provider)
                            .filter(|_| !disabled)
                        {
                            let key = validate_key(key)?;
                            write_private_file(&path, key.as_bytes())
                                .map_err(|_| CredentialError::Storage)?;
                            Some(key.to_owned())
                        } else {
                            None
                        }
                    }
                    Err(_) => return Err(CredentialError::Storage),
                };
                if let Some(key) = key {
                    api_keys.insert((*provider).to_owned(), key);
                }
            }
            let chatgpt_path = read_directory.join("codex-chatgpt.json");
            let mut account_ids = Vec::new();
            match std::fs::symlink_metadata(&chatgpt_path) {
                Ok(_) => {
                    open_private_file(&chatgpt_path).map_err(|_| CredentialError::Storage)?;
                    account_ids.push("default".to_owned());
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(CredentialError::Storage),
            }
            for entry in std::fs::read_dir(&read_directory).map_err(|_| CredentialError::Storage)? {
                let entry = entry.map_err(|_| CredentialError::Storage)?;
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                let Some(id) = name
                    .strip_prefix("codex-chatgpt-")
                    .and_then(|name| name.strip_suffix(".json"))
                else {
                    continue;
                };
                if !valid_account_id(id) || id == "default" || account_ids.len() >= 16 {
                    return Err(CredentialError::Storage);
                }
                open_private_file(&entry.path()).map_err(|_| CredentialError::Storage)?;
                account_ids.push(id.to_owned());
            }
            Ok((api_keys, account_ids))
        })
        .await
        .map_err(|_| CredentialError::Storage)??;
        let mut accounts = HashMap::new();
        for id in account_ids {
            let connection = Arc::new(
                ChatGptConnection::load(account_path(&directory, &id))
                    .await
                    .map_err(|_| CredentialError::Storage)?,
            );
            accounts.insert(id, connection);
        }
        let selection_path = directory.join("codex-active-account");
        let mut active_account = match std::fs::symlink_metadata(&selection_path) {
            Ok(_) => {
                let mut value = String::new();
                open_private_file(&selection_path)
                    .map_err(|_| CredentialError::Storage)?
                    .take(65)
                    .read_to_string(&mut value)
                    .map_err(|_| CredentialError::Storage)?;
                if !valid_account_id(&value) || !accounts.contains_key(&value) {
                    return Err(CredentialError::Storage);
                }
                Some(value)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => accounts
                .contains_key("default")
                .then(|| "default".to_owned()),
            Err(_) => return Err(CredentialError::Storage),
        };
        let mut chatgpt =
            if let Some(connection) = active_account.as_ref().and_then(|id| accounts.get(id)) {
                connection
                    .authenticated()
                    .await
                    .then(|| Arc::clone(connection))
            } else {
                None
            };
        let mode_path = directory.join("codex-auth-mode");
        let saved_mode = match std::fs::symlink_metadata(&mode_path) {
            Ok(_) => {
                let mut bytes = Vec::new();
                open_private_file(&mode_path)
                    .map_err(|_| CredentialError::Storage)?
                    .take(1025)
                    .read_to_end(&mut bytes)
                    .map_err(|_| CredentialError::Storage)?;
                if bytes.len() > 1024 {
                    return Err(CredentialError::Storage);
                }
                if let Ok(selection) = serde_json::from_slice::<CodexSelection>(&bytes) {
                    if selection
                        .auth_mode
                        .as_deref()
                        .is_some_and(|mode| !matches!(mode, "api_key" | "chatgpt"))
                        || selection
                            .account_id
                            .as_ref()
                            .is_some_and(|id| !valid_account_id(id) || !accounts.contains_key(id))
                        || selection.auth_mode.as_deref() == Some("chatgpt")
                            && selection.account_id.is_none()
                    {
                        return Err(CredentialError::Storage);
                    }
                    active_account = selection.account_id;
                    chatgpt = if let Some(connection) =
                        active_account.as_ref().and_then(|id| accounts.get(id))
                    {
                        connection
                            .authenticated()
                            .await
                            .then(|| Arc::clone(connection))
                    } else {
                        None
                    };
                    selection.auth_mode
                } else {
                    // Read the legacy single-mode file without silently
                    // falling back when a saved record is malformed.
                    let mode = String::from_utf8(bytes).map_err(|_| CredentialError::Storage)?;
                    if !matches!(mode.as_str(), "api_key" | "chatgpt" | "disabled") {
                        return Err(CredentialError::Storage);
                    }
                    (mode != "disabled").then_some(mode)
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if chatgpt.is_some() {
                    Some("chatgpt".to_owned())
                } else if api_keys.contains_key("codex") {
                    Some("api_key".to_owned())
                } else {
                    None
                }
            }
            Err(_) => return Err(CredentialError::Storage),
        };
        let (shutdown, _) = watch::channel(false);
        let go_key = api_keys.get("opencode-go").cloned();
        Ok((
            Arc::new(Self {
                directory,
                host_id,
                zen_base_url,
                codex_base_url,
                shutdown,
                state: Mutex::new(CredentialState {
                    api_keys,
                    chatgpt,
                    accounts,
                    active_account,
                    active_codex_auth: saved_mode,
                    login: None,
                    stopping: false,
                }),
            }),
            go_key,
        ))
    }

    /// Build the complete provider set before the scheduler starts claiming work.
    pub async fn provider_clients(
        &self,
        go_base_url: Option<&str>,
    ) -> Result<HashMap<String, Arc<dyn ProviderClient>>, CredentialError> {
        let state = self.state.lock().await;
        let mut clients = HashMap::new();
        for (provider, key) in &state.api_keys {
            if provider == "codex" && state.active_codex_auth.is_none() {
                continue;
            }
            if provider == "codex" && state.active_codex_auth.as_deref() == Some("chatgpt") {
                continue;
            }
            let client: Arc<dyn ProviderClient> = match provider.as_str() {
                "opencode-go" => Arc::new(
                    match go_base_url {
                        Some(url) => OpencodeGoClient::new_with_base_url(key, url),
                        None => OpencodeGoClient::new(key),
                    }
                    .map_err(|_| CredentialError::InvalidKey)?,
                ),
                "opencode-zen" => Arc::new(
                    match self.zen_base_url.as_deref() {
                        Some(url) => OpencodeZenClient::new_with_base_url(key, url),
                        None => OpencodeZenClient::new(key),
                    }
                    .map_err(|_| CredentialError::InvalidKey)?,
                ),
                "codex" => Arc::new(
                    match self.codex_base_url.as_deref() {
                        Some(url) => CodexClient::new_with_base_url(key, url),
                        None => CodexClient::new(key),
                    }
                    .map_err(|_| CredentialError::InvalidKey)?,
                ),
                _ => continue,
            };
            clients.insert(provider.clone(), client);
        }
        if let Some(connection) = &state.chatgpt
            && state.active_codex_auth.as_deref() == Some("chatgpt")
        {
            let client: Arc<dyn ProviderClient> = Arc::new(
                match self.codex_base_url.as_deref() {
                    Some(url) => {
                        CodexClient::from_chatgpt_with_base_url(Arc::clone(connection), url)
                    }
                    None => CodexClient::from_chatgpt(Arc::clone(connection)),
                }
                .map_err(|_| CredentialError::Storage)?,
            );
            clients.insert("codex".to_owned(), client);
        }
        Ok(clients)
    }

    pub async fn statuses(&self) -> Vec<ProviderStatus> {
        let state = self.state.lock().await;
        PROVIDERS
            .iter()
            .map(|(provider, _)| status(&state, provider))
            .collect()
    }

    pub async fn accounts(&self) -> Vec<ProviderAccount> {
        let (accounts, active) = {
            let state = self.state.lock().await;
            (
                state.accounts.clone(),
                (state.active_codex_auth.as_deref() == Some("chatgpt"))
                    .then(|| state.active_account.clone())
                    .flatten(),
            )
        };
        let mut result = Vec::new();
        for (id, connection) in accounts {
            result.push(ProviderAccount {
                active: active.as_deref() == Some(&id),
                id,
                authenticated: connection.authenticated().await,
            });
        }
        result.sort_by(|a, b| a.id.cmp(&b.id));
        result
    }

    /// Capture discovery scope and provider connection under the same lock used
    /// for activation, so an account switch cannot relabel an in-flight probe.
    pub async fn discovery_client(
        &self,
        provider: &str,
        runtime: &ChatRuntime,
    ) -> Result<(Arc<dyn ProviderClient>, Option<String>), CredentialError> {
        check_provider(provider)?;
        let state = self.state.lock().await;
        let client = runtime
            .provider_client(provider)
            .ok_or(CredentialError::Unavailable)?;
        let account = (provider == "codex"
            && state.active_codex_auth.as_deref() == Some("chatgpt"))
        .then(|| state.active_account.clone())
        .flatten();
        Ok((client, account))
    }

    pub async fn select_auth(
        self: &Arc<Self>,
        request: SelectAuthRequest,
        runtime: ChatRuntime,
    ) -> Result<ProviderStatus, CredentialError> {
        let owner = Arc::clone(self);
        tokio::spawn(async move {
            let mut state = owner.state.lock().await;
            if state.stopping {
                return Err(CredentialError::Stopping);
            }
            if state
                .login
                .as_ref()
                .is_some_and(|login| login.status.status == "pending")
            {
                return Err(CredentialError::LoginPending);
            }
            let (client, connection, account): (Arc<dyn ProviderClient>, _, _) =
                match request.auth_mode.as_str() {
                    "api_key" if request.account_id.is_none() => {
                        let key = state
                            .api_keys
                            .get("codex")
                            .ok_or(CredentialError::Unavailable)?;
                        let client = match owner.codex_base_url.as_deref() {
                            Some(url) => CodexClient::new_with_base_url(key, url),
                            None => CodexClient::new(key),
                        }
                        .map_err(|_| CredentialError::Storage)?;
                        (
                            Arc::new(client),
                            state.chatgpt.clone(),
                            state.active_account.clone(),
                        )
                    }
                    "chatgpt" => {
                        let id = request
                            .account_id
                            .map(|id| id.to_ascii_lowercase())
                            .or_else(|| state.active_account.clone())
                            .ok_or(CredentialError::Unavailable)?;
                        if !valid_account_id(&id) {
                            return Err(CredentialError::InvalidId);
                        }
                        let connection = state
                            .accounts
                            .get(&id)
                            .cloned()
                            .ok_or(CredentialError::NotFound)?;
                        if !connection.authenticated().await {
                            return Err(CredentialError::Unavailable);
                        }
                        let client = match owner.codex_base_url.as_deref() {
                            Some(url) => CodexClient::from_chatgpt_with_base_url(
                                Arc::clone(&connection),
                                url,
                            ),
                            None => CodexClient::from_chatgpt(Arc::clone(&connection)),
                        }
                        .map_err(|_| CredentialError::Storage)?;
                        (Arc::new(client), Some(connection), Some(id))
                    }
                    _ => return Err(CredentialError::Unavailable),
                };
            let path = owner.directory.join("codex-auth-mode");
            let selection = account.clone();
            let mode = request.auth_mode.clone();
            tokio::task::spawn_blocking(move || {
                write_codex_selection(&path, Some(&mode), selection.as_deref())
            })
            .await
            .map_err(|_| CredentialError::Storage)?
            .map_err(|_| CredentialError::Storage)?;
            state.active_account = account;
            state.chatgpt = connection;
            state.active_codex_auth = Some(request.auth_mode);
            runtime.replace_provider(client);
            Ok(status(&state, "codex"))
        })
        .await
        .map_err(|_| CredentialError::Storage)?
    }

    pub async fn sign_out(
        self: &Arc<Self>,
        account_id: String,
        runtime: ChatRuntime,
    ) -> Result<SignOutResponse, CredentialError> {
        let account_id = account_id.to_ascii_lowercase();
        if !valid_account_id(&account_id) {
            return Err(CredentialError::InvalidId);
        }
        let owner = Arc::clone(self);
        tokio::spawn(async move {
            let mut state = owner.state.lock().await;
            if state.stopping {
                return Err(CredentialError::Stopping);
            }
            if state
                .login
                .as_ref()
                .is_some_and(|login| login.status.status == "pending")
            {
                return Err(CredentialError::LoginPending);
            }
            let connection = state
                .accounts
                .get(&account_id)
                .cloned()
                .ok_or(CredentialError::NotFound)?;
            if state.active_account.as_deref() == Some(&account_id) {
                if state.active_codex_auth.as_deref() == Some("chatgpt") {
                    let path = owner.directory.join("codex-auth-mode");
                    let selected = state.active_account.clone();
                    tokio::task::spawn_blocking(move || {
                        write_codex_selection(&path, None, selected.as_deref())
                    })
                    .await
                    .map_err(|_| CredentialError::Storage)?
                    .map_err(|_| CredentialError::Storage)?;
                    state.active_codex_auth = None;
                    runtime.remove_provider("codex");
                }
                state.chatgpt = None;
            }
            let confirmed = connection
                .sign_out()
                .await
                .map_err(|_| CredentialError::Storage)?;
            Ok(SignOutResponse {
                account_id,
                remote_revocation_confirmed: confirmed,
            })
        })
        .await
        .map_err(|_| CredentialError::Storage)?
    }

    pub async fn remove_api_key(
        self: &Arc<Self>,
        provider: String,
        runtime: ChatRuntime,
    ) -> Result<ProviderStatus, CredentialError> {
        check_provider(&provider)?;
        let owner = Arc::clone(self);
        tokio::spawn(async move {
            let mut state = owner.state.lock().await;
            if state.stopping {
                return Err(CredentialError::Stopping);
            }
            let path = key_path(&owner.directory, &provider);
            let tombstone = owner.directory.join(format!("{provider}-api-key-disabled"));
            let mode_path = owner.directory.join("codex-auth-mode");
            let selected = state.active_account.clone();
            let disable_codex =
                provider == "codex" && state.active_codex_auth.as_deref() == Some("api_key");
            tokio::task::spawn_blocking(move || -> Result<(), CredentialError> {
                write_private_file(&tombstone, b"disabled")
                    .map_err(|_| CredentialError::Storage)?;
                if disable_codex {
                    write_codex_selection(&mode_path, None, selected.as_deref())
                        .map_err(|_| CredentialError::Storage)?;
                }
                match std::fs::remove_file(&path) {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(_) => Err(CredentialError::Storage),
                }
            })
            .await
            .map_err(|_| CredentialError::Storage)?
            .map_err(|_| CredentialError::Storage)?;
            state.api_keys.remove(&provider);
            if provider != "codex" || disable_codex {
                runtime.remove_provider(&provider);
                if provider == "codex" {
                    state.active_codex_auth = None;
                }
            }
            Ok(status(&state, &provider))
        })
        .await
        .map_err(|_| CredentialError::Storage)?
    }

    pub async fn set_api_key(
        self: &Arc<Self>,
        provider: String,
        key: String,
        runtime: ChatRuntime,
    ) -> Result<ProviderStatus, CredentialError> {
        check_provider(&provider)?;
        let key = validate_key(&key)?.to_owned();
        // This task owns persistence + activation even if its HTTP caller exits.
        let owner = Arc::clone(self);
        tokio::spawn(async move {
            let mut state = owner.state.lock().await;
            if state.stopping {
                return Err(CredentialError::Stopping);
            }
            let client: Arc<dyn ProviderClient> = match provider.as_str() {
                "opencode-go" => runtime
                    .prepare_api_key(&key)
                    .map_err(|_| CredentialError::InvalidKey)?,
                "opencode-zen" => Arc::new(
                    match owner.zen_base_url.as_deref() {
                        Some(url) => OpencodeZenClient::new_with_base_url(&key, url),
                        None => OpencodeZenClient::new(&key),
                    }
                    .map_err(|_| CredentialError::InvalidKey)?,
                ),
                "codex" => Arc::new(
                    match owner.codex_base_url.as_deref() {
                        Some(url) => CodexClient::new_with_base_url(&key, url),
                        None => CodexClient::new(&key),
                    }
                    .map_err(|_| CredentialError::InvalidKey)?,
                ),
                _ => return Err(CredentialError::Unsupported),
            };
            let path = key_path(&owner.directory, &provider);
            let persisted_key = key.clone();
            tokio::task::spawn_blocking(move || {
                write_private_file(&path, persisted_key.as_bytes())
            })
            .await
            .map_err(|_| CredentialError::Storage)?
            .map_err(|_| CredentialError::Storage)?;
            if provider == "codex" {
                let mode_path = owner.directory.join("codex-auth-mode");
                let selected = state.active_account.clone();
                tokio::task::spawn_blocking(move || {
                    write_codex_selection(&mode_path, Some("api_key"), selected.as_deref())
                })
                .await
                .map_err(|_| CredentialError::Storage)?
                .map_err(|_| CredentialError::Storage)?;
                state.active_codex_auth = Some("api_key".to_owned());
            }
            runtime.replace_provider(client);
            state.api_keys.insert(provider.clone(), key);
            Ok(status(&state, &provider))
        })
        .await
        .map_err(|_| CredentialError::Storage)?
    }

    #[cfg(test)]
    pub async fn start_login(
        self: &Arc<Self>,
        command_id: String,
        runtime: Option<ChatRuntime>,
    ) -> Result<LoginResponse, CredentialError> {
        self.start_login_for_account(command_id, None, runtime)
            .await
    }

    pub async fn start_login_for_account(
        self: &Arc<Self>,
        command_id: String,
        account_id: Option<String>,
        runtime: Option<ChatRuntime>,
    ) -> Result<LoginResponse, CredentialError> {
        if command_id.is_empty()
            || command_id.len() > 128
            || !command_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(CredentialError::InvalidId);
        }
        let mut state = self.state.lock().await;
        if state.stopping {
            return Err(CredentialError::Stopping);
        }
        let account_id = account_id
            .map(|id| id.to_ascii_lowercase())
            .or_else(|| state.active_account.clone())
            .unwrap_or_else(|| "default".to_owned());
        if !valid_account_id(&account_id)
            || !state.accounts.contains_key(&account_id) && state.accounts.len() >= 16
        {
            return Err(CredentialError::InvalidId);
        }
        if let Some(login) = &state.login {
            if login.response.login_id == command_id {
                if login.account_id != account_id {
                    return Err(CredentialError::LoginPending);
                }
                return Ok(login.response.clone());
            }
            if login.status.status == "pending" {
                return Err(CredentialError::LoginPending);
            }
        }
        let existing = state.accounts.get(&account_id).cloned();
        let login = match &existing {
            Some(connection) => connection.begin_login().await,
            None => ChatGptLogin::begin(&self.host_id).await,
        }
        .map_err(|_| CredentialError::LoginFailed)?;
        let response = LoginResponse {
            login_id: command_id.clone(),
            authorization_url: login.authorization_url().to_owned(),
        };
        let status = LoginStatus {
            login_id: command_id,
            status: "pending".to_owned(),
            error_code: None,
        };
        let owner = Arc::clone(self);
        let mut shutdown = self.shutdown.subscribe();
        let login_account = account_id.clone();
        let task = tokio::spawn(async move {
            let outcome = if let Some(connection) = existing {
                connection
                    .finish_login_into(login, async {
                        if !*shutdown.borrow() {
                            let _ = shutdown.changed().await;
                        }
                    })
                    .await
                    .map(|()| connection)
            } else {
                login
                    .finish_with_cancellation(
                        account_path(&owner.directory, &login_account),
                        async {
                            if !*shutdown.borrow() {
                                let _ = shutdown.changed().await;
                            }
                        },
                    )
                    .await
                    .map(Arc::new)
            };
            let mut state = owner.state.lock().await;
            let succeeded = match outcome {
                Ok(connection) => {
                    state
                        .accounts
                        .insert(login_account.clone(), Arc::clone(&connection));
                    let mode_path = owner.directory.join("codex-auth-mode");
                    let selection = login_account.clone();
                    let persisted = tokio::task::spawn_blocking(move || {
                        write_codex_selection(&mode_path, Some("chatgpt"), Some(&selection))
                    })
                    .await
                    .is_ok_and(|result| result.is_ok());
                    if !persisted {
                        if let Some(login) = &mut state.login {
                            login.status.status = "failed".to_owned();
                            login.status.error_code =
                                Some("credential_storage_unavailable".to_owned());
                        }
                        return;
                    }
                    state.active_codex_auth = Some("chatgpt".to_owned());
                    state.active_account = Some(login_account);
                    state.chatgpt = Some(Arc::clone(&connection));
                    let client_result = match owner.codex_base_url.as_deref() {
                        Some(url) => {
                            CodexClient::from_chatgpt_with_base_url(Arc::clone(&connection), url)
                        }
                        None => CodexClient::from_chatgpt(Arc::clone(&connection)),
                    };
                    if let (Some(runtime), Ok(client)) = (runtime.as_ref(), client_result) {
                        runtime.replace_provider(Arc::new(client));
                    }
                    true
                }
                Err(_) => false,
            };
            if let Some(login) = &mut state.login {
                login.status.status = if succeeded { "succeeded" } else { "failed" }.to_owned();
                login.status.error_code = (!succeeded).then(|| "login_failed".to_owned());
            }
        });
        state.login = Some(LoginRecord {
            account_id,
            response: response.clone(),
            status,
            task: Some(task),
        });
        Ok(response)
    }

    pub async fn login_status(&self, id: &str) -> Result<LoginStatus, CredentialError> {
        self.state
            .lock()
            .await
            .login
            .as_ref()
            .filter(|login| login.status.login_id == id)
            .map(|login| login.status.clone())
            .ok_or(CredentialError::NotFound)
    }

    pub async fn shutdown(&self) {
        let task = {
            let mut state = self.state.lock().await;
            state.stopping = true;
            self.shutdown.send_replace(true);
            state.login.as_mut().and_then(|login| login.task.take())
        };
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn selection_restores_mode_and_account_together_and_rejects_unknown_accounts() {
        let directory = tempfile::tempdir().unwrap();
        let credentials = directory.path().join("credentials");
        create_private_dir(&credentials).unwrap();
        for id in ["default", "work"] {
            let record = serde_json::json!({
                "issuer":"https://auth.openai.com", "subject":id,
                "client_id":format!("oaiapp_{id}"), "ext_agent_host_id":"host",
                "id_token":"", "access_token":"", "refresh_token":"",
                "scopes":[], "expires_at":0,
            });
            write_private_file(
                &account_path(&credentials, id),
                &serde_json::to_vec(&record).unwrap(),
            )
            .unwrap();
        }
        let selection_path = credentials.join("codex-auth-mode");
        write_codex_selection(&selection_path, None, Some("work")).unwrap();
        let (owner, _) = ProviderCredentials::load(directory.path(), "host".into(), vec![])
            .await
            .unwrap();
        let state = owner.state.lock().await;
        assert_eq!(state.active_account.as_deref(), Some("work"));
        assert!(state.active_codex_auth.is_none());
        drop(state);
        assert!(owner.provider_clients(None).await.unwrap().is_empty());
        write_codex_selection(&selection_path, Some("chatgpt"), Some("unknown")).unwrap();
        assert!(matches!(
            ProviderCredentials::load(directory.path(), "host".into(), vec![]).await,
            Err(CredentialError::Storage)
        ));
    }

    #[tokio::test]
    async fn bootstrap_is_private_and_saved_keys_override_environment() {
        let directory = tempfile::tempdir().unwrap();
        let (owner, key) = ProviderCredentials::load(
            directory.path(),
            "host-1".into(),
            vec![("opencode-go".into(), "initial-key".into())],
        )
        .await
        .unwrap();
        assert_eq!(key.as_deref(), Some("initial-key"));
        assert!(owner.statuses().await[0].api_key_configured);
        let path = directory.path().join("credentials/opencode-go-api-key");
        write_private_file(&path, b"replacement-key").unwrap();
        let (_, key) = ProviderCredentials::load(
            directory.path(),
            "host-1".into(),
            vec![("opencode-go".into(), "stale-env-key".into())],
        )
        .await
        .unwrap();
        assert_eq!(key.as_deref(), Some("replacement-key"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        owner.shutdown().await;
    }

    #[tokio::test]
    async fn duplicate_login_reuses_callback_and_shutdown_cancels_it() {
        let directory = tempfile::tempdir().unwrap();
        let (owner, _) = ProviderCredentials::load(directory.path(), "stable-host".into(), vec![])
            .await
            .unwrap();
        let first = owner.start_login("login-1".into(), None).await.unwrap();
        let repeated = owner.start_login("login-1".into(), None).await.unwrap();
        assert_eq!(first.authorization_url, repeated.authorization_url);
        assert!(
            first
                .authorization_url
                .contains("ext_agent_host_id=stable-host")
        );
        assert!(matches!(
            owner.start_login("login-2".into(), None).await,
            Err(CredentialError::LoginPending)
        ));
        tokio::time::timeout(std::time::Duration::from_secs(2), owner.shutdown())
            .await
            .unwrap();
        assert_eq!(
            owner.login_status("login-1").await.unwrap().status,
            "failed"
        );
        assert!(
            !directory
                .path()
                .join("credentials/codex-chatgpt.json")
                .exists()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn insecure_or_symlink_credentials_fail_closed() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let directory = tempfile::tempdir().unwrap();
        let credentials = directory.path().join("credentials");
        create_private_dir(&credentials).unwrap();
        let path = credentials.join("opencode-go-api-key");
        std::fs::write(&path, "key").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            ProviderCredentials::load(directory.path(), "host".into(), vec![])
                .await
                .is_err()
        );
        assert!(write_private_file(&path, b"new-key").is_err());
        std::fs::remove_file(&path).unwrap();
        symlink(directory.path().join("missing"), &path).unwrap();
        assert!(
            ProviderCredentials::load(directory.path(), "host".into(), vec![])
                .await
                .is_err()
        );
        assert!(write_private_file(&path, b"new-key").is_err());
    }
}
