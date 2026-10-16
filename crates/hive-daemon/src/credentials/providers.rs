use super::*;

impl ProviderCredentials {
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
}
