use super::credentials::{Credentials, auth_error, now, save, token_exchange};
use super::*;

pub(super) struct TokenState {
    pub(super) credentials: Credentials,
    refresh_blocked: bool,
}

/// One account registration. Share this object across clients/sessions so token
/// rotation is serialized. The daemon must exclusively own its credential file.
pub struct ChatGptConnection {
    http: reqwest::Client,
    pub(super) token_endpoint: String,
    pub(super) discovery_endpoint: String,
    path: PathBuf,
    pub(super) state: Mutex<TokenState>,
}

impl ChatGptConnection {
    /// Stable registration identity for private continuation. Token rotation
    /// must not invalidate context, while another registration must not reuse it.
    pub(in crate::providers) async fn continuation_identity(&self) -> String {
        let state = self.state.lock().await;
        let c = &state.credentials;
        let mut hash = Sha256::new();
        for value in [&c.issuer, &c.subject, &c.client_id, &c.ext_agent_host_id] {
            hash.update(value.as_bytes());
            hash.update([0]);
        }
        format!("{:x}", hash.finalize())
    }

    /// Restore a credential record created by a validated login in this app.
    pub async fn load(path: impl AsRef<Path>) -> Result<Self, ProviderError> {
        let path = path.as_ref().to_owned();
        let read_path = path.clone();
        let credentials: Credentials = tokio::task::spawn_blocking(move || {
            let file =
                std::fs::File::open(read_path).map_err(|_| ProviderError::CredentialStorage)?;
            let mut body = Vec::new();
            file.take(MAX_CREDENTIAL_BYTES + 1)
                .read_to_end(&mut body)
                .map_err(|_| ProviderError::CredentialStorage)?;
            if body.len() as u64 > MAX_CREDENTIAL_BYTES {
                return Err(ProviderError::CredentialStorage);
            }
            serde_json::from_slice(&body).map_err(|_| ProviderError::CredentialStorage)
        })
        .await
        .map_err(|_| ProviderError::CredentialStorage)??;
        credentials.validate_registration()?;
        if !credentials.signed_out() {
            credentials.validate()?;
        }
        Ok(Self::from_credentials(
            path,
            credentials,
            transport::http_client()?,
        ))
    }

    pub(super) fn from_credentials(
        path: PathBuf,
        credentials: Credentials,
        http: reqwest::Client,
    ) -> Self {
        Self {
            http,
            path,
            token_endpoint: TOKEN_URL.to_owned(),
            discovery_endpoint: format!("{ISSUER}/.well-known/openid-configuration"),
            state: Mutex::new(TokenState {
                credentials,
                refresh_blocked: false,
            }),
        }
    }

    /// Reauthorize the selected registration with its saved client and host ids.
    pub async fn begin_login(&self) -> Result<ChatGptLogin, ProviderError> {
        let state = self.state.lock().await;
        ChatGptLogin::start(
            &state.credentials.ext_agent_host_id,
            Some(state.credentials.clone()),
        )
        .await
    }

    /// Reauthorize this registration through a pasted callback, without binding
    /// a local listener. The existing account/client and host identity are kept.
    pub async fn begin_headless_login(
        &self,
    ) -> Result<(ChatGptLogin, ChatGptCallback), ProviderError> {
        let state = self.state.lock().await;
        ChatGptLogin::start_headless(
            &state.credentials.ext_agent_host_id,
            Some(state.credentials.clone()),
        )
        .await
    }

    /// Complete a returning login into this shared registration. Holding the
    /// token lock across persistence prevents an active refresh from overwriting
    /// credentials installed by the newer authorization.
    pub async fn finish_login_into(
        &self,
        login: ChatGptLogin,
        cancellation: impl std::future::Future<Output = ()>,
    ) -> Result<(), ProviderError> {
        let credentials = tokio::select! {
            biased;
            _ = cancellation => return Err(auth_error("login interrupted")),
            credentials = login.authorize() => credentials?,
        };
        let mut state = self.state.lock().await;
        save(self.path.clone(), credentials.clone()).await?;
        state.credentials = credentials;
        state.refresh_blocked = false;
        Ok(())
    }

    pub(in crate::providers) async fn access_token(&self) -> Result<String, ProviderError> {
        let mut state = self.state.lock().await;
        state.credentials.validate()?;
        if state.credentials.expires_at > now()?.saturating_add(60) {
            return Ok(state.credentials.access_token.clone());
        }
        if state.refresh_blocked {
            return Err(auth_error("token renewal requires a new sign-in"));
        }
        // If dispatch, parsing, or persistence fails, a rotating refresh token
        // may have been consumed. Never automatically replay that operation.
        state.refresh_blocked = true;
        let credentials = &state.credentials;
        let token = token_exchange(
            &self.http,
            &self.token_endpoint,
            &[
                ("grant_type", "refresh_token"),
                ("client_id", &credentials.client_id),
                ("refresh_token", &credentials.refresh_token),
                ("resource", RESOURCE),
            ],
        )
        .await?;
        let mut replacement = state.credentials.clone();
        token.apply(&mut replacement)?;
        save(self.path.clone(), replacement.clone()).await?;
        state.credentials = replacement;
        state.refresh_blocked = false;
        Ok(state.credentials.access_token.clone())
    }

    pub async fn authenticated(&self) -> bool {
        !self.state.lock().await.credentials.signed_out()
    }

    /// Attempt documented revocation once, then clear local tokens atomically.
    /// A false result means revocation was not confirmed. Registration identity
    /// remains available for returning authorization with the issued client ID.
    pub async fn sign_out(&self) -> Result<bool, ProviderError> {
        let mut state = self.state.lock().await;
        if state.credentials.signed_out() {
            // A prior write may have failed after tokens were cleared in
            // memory. Retry only that safe local write, never remote revocation.
            save(self.path.clone(), state.credentials.clone()).await?;
            return Ok(state.credentials.revocation_confirmed);
        }
        state.refresh_blocked = true;
        let confirmed = self.revoke(&state.credentials).await.unwrap_or(false);
        let mut replacement = state.credentials.clone();
        replacement.id_token.clear();
        replacement.access_token.clear();
        replacement.refresh_token.clear();
        replacement.scopes.clear();
        replacement.expires_at = 0;
        replacement.revocation_confirmed = confirmed;
        // Even a failed disk write must prevent this connection using tokens.
        state.credentials = replacement.clone();
        save(self.path.clone(), replacement).await?;
        Ok(confirmed)
    }

    async fn revoke(&self, credentials: &Credentials) -> Result<bool, ProviderError> {
        let response = self
            .http
            .get(&self.discovery_endpoint)
            .timeout(Duration::from_secs(10))
            .send()
            .await?;
        let body = transport::read_body_limited(super::ProviderId::Codex, response).await?;
        let discovery: serde_json::Value =
            serde_json::from_str(&body).map_err(|_| auth_error("invalid OIDC discovery"))?;
        let endpoint = discovery
            .get("revocation_endpoint")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| auth_error("OIDC discovery has no revocation endpoint"))?;
        let endpoint =
            reqwest::Url::parse(endpoint).map_err(|_| auth_error("invalid revocation endpoint"))?;
        let discovery_url = reqwest::Url::parse(&self.discovery_endpoint)
            .map_err(|_| auth_error("invalid discovery endpoint"))?;
        if endpoint.origin() != discovery_url.origin()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(auth_error("untrusted revocation endpoint"));
        }
        let response = self
            .http
            .post(endpoint)
            .timeout(Duration::from_secs(10))
            .form(&[
                ("token", credentials.refresh_token.as_str()),
                ("token_type_hint", "refresh_token"),
                ("client_id", credentials.client_id.as_str()),
            ])
            .send()
            .await?;
        Ok(response.status() == reqwest::StatusCode::OK)
    }
}

#[cfg(test)]
pub(in crate::providers) fn fixture_connection(
    path: PathBuf,
    expires_at: u64,
) -> ChatGptConnection {
    let credentials = Credentials {
        issuer: ISSUER.to_owned(),
        subject: "fixture-subject".to_owned(),
        client_id: "oaiapp_fixture".to_owned(),
        ext_agent_host_id: "fixture-host".to_owned(),
        id_token: "fixture-id-token".to_owned(),
        access_token: "fixture-access-token".to_owned(),
        refresh_token: "fixture-refresh-token".to_owned(),
        scopes: SCOPES.split_whitespace().map(str::to_owned).collect(),
        expires_at,
        revocation_confirmed: false,
    };
    ChatGptConnection::from_credentials(path, credentials, transport::http_client().unwrap())
}
