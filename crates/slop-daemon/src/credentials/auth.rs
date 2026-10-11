use super::*;

impl ProviderCredentials {
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
}
