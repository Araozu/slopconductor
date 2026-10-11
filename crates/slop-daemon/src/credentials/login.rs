use super::*;

impl ProviderCredentials {
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
