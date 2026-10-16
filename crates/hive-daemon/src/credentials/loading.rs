use super::*;

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
}
