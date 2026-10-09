//! Daemon-owned provider secrets, separate from conversation data and events.

use std::{
    collections::HashSet,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

use slop_protocol::providers::{LoginResponse, LoginStatus, MAX_API_KEY_BYTES, ProviderStatus};
use slop_runtime::{
    chat::ChatRuntime,
    providers::{
        chatgpt_auth::{ChatGptConnection, ChatGptLogin},
        codex::CodexClient,
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
}

pub struct ProviderCredentials {
    directory: PathBuf,
    host_id: String,
    state: Mutex<CredentialState>,
    shutdown: watch::Sender<bool>,
}

struct CredentialState {
    api_keys: HashSet<String>,
    chatgpt: Option<Arc<ChatGptConnection>>,
    login: Option<LoginRecord>,
    stopping: bool,
}

struct LoginRecord {
    response: LoginResponse,
    status: LoginStatus,
    task: Option<tokio::task::JoinHandle<()>>,
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
        api_key_configured: state.api_keys.contains(provider),
        chatgpt_configured: provider == "codex" && state.chatgpt.is_some(),
        execution_supported: provider == "opencode-go",
    }
}

impl ProviderCredentials {
    /// Restore private records. Legacy environment keys are imported only when
    /// a stored key does not exist; saved runtime changes win on every restart.
    pub async fn load(
        data_dir: &Path,
        host_id: String,
        bootstrap: Vec<(String, String)>,
    ) -> Result<(Arc<Self>, Option<String>), CredentialError> {
        let directory = data_dir.join("credentials");
        let read_directory = directory.clone();
        let (api_keys, go_key, has_chatgpt) = tokio::task::spawn_blocking(move || {
            create_private_dir(&read_directory).map_err(|_| CredentialError::Storage)?;
            let mut api_keys = HashSet::new();
            let mut go_key = None;
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
                        if let Some((_, key)) = bootstrap.iter().find(|(id, _)| id == provider) {
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
                    api_keys.insert((*provider).to_owned());
                    if *provider == "opencode-go" {
                        go_key = Some(key);
                    }
                }
            }
            let chatgpt_path = read_directory.join("codex-chatgpt.json");
            let has_chatgpt = match std::fs::symlink_metadata(&chatgpt_path) {
                Ok(_) => {
                    open_private_file(&chatgpt_path).map_err(|_| CredentialError::Storage)?;
                    true
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                Err(_) => return Err(CredentialError::Storage),
            };
            Ok((api_keys, go_key, has_chatgpt))
        })
        .await
        .map_err(|_| CredentialError::Storage)??;
        let chatgpt = if has_chatgpt {
            Some(Arc::new(
                ChatGptConnection::load(directory.join("codex-chatgpt.json"))
                    .await
                    .map_err(|_| CredentialError::Storage)?,
            ))
        } else {
            None
        };
        let (shutdown, _) = watch::channel(false);
        Ok((
            Arc::new(Self {
                directory,
                host_id,
                shutdown,
                state: Mutex::new(CredentialState {
                    api_keys,
                    chatgpt,
                    login: None,
                    stopping: false,
                }),
            }),
            go_key,
        ))
    }

    pub async fn statuses(&self) -> Vec<ProviderStatus> {
        let state = self.state.lock().await;
        PROVIDERS
            .iter()
            .map(|(provider, _)| status(&state, provider))
            .collect()
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
            let go_client = match provider.as_str() {
                "opencode-go" => Some(
                    runtime
                        .prepare_api_key(&key)
                        .map_err(|_| CredentialError::InvalidKey)?,
                ),
                "opencode-zen" => {
                    OpencodeZenClient::new(&key).map_err(|_| CredentialError::InvalidKey)?;
                    None
                }
                "codex" => {
                    CodexClient::new(&key).map_err(|_| CredentialError::InvalidKey)?;
                    None
                }
                _ => return Err(CredentialError::Unsupported),
            };
            let path = key_path(&owner.directory, &provider);
            tokio::task::spawn_blocking(move || write_private_file(&path, key.as_bytes()))
                .await
                .map_err(|_| CredentialError::Storage)?
                .map_err(|_| CredentialError::Storage)?;
            if let Some(client) = go_client {
                runtime.replace_provider(client);
            }
            state.api_keys.insert(provider.clone());
            Ok(status(&state, &provider))
        })
        .await
        .map_err(|_| CredentialError::Storage)?
    }

    pub async fn start_login(
        self: &Arc<Self>,
        command_id: String,
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
        if let Some(login) = &state.login {
            if login.response.login_id == command_id {
                return Ok(login.response.clone());
            }
            if login.status.status == "pending" {
                return Err(CredentialError::LoginPending);
            }
        }
        let login = match &state.chatgpt {
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
        let task = tokio::spawn(async move {
            let outcome = login
                .finish_with_cancellation(owner.directory.join("codex-chatgpt.json"), async {
                    if !*shutdown.borrow() {
                        let _ = shutdown.changed().await;
                    }
                })
                .await;
            let mut state = owner.state.lock().await;
            let succeeded = match outcome {
                Ok(connection) => {
                    state.chatgpt = Some(Arc::new(connection));
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
        let first = owner.start_login("login-1".into()).await.unwrap();
        let repeated = owner.start_login("login-1".into()).await.unwrap();
        assert_eq!(first.authorization_url, repeated.authorization_url);
        assert!(
            first
                .authorization_url
                .contains("ext_agent_host_id=stable-host")
        );
        assert!(matches!(
            owner.start_login("login-2".into()).await,
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
