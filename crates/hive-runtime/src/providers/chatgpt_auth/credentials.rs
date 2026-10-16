use super::*;

pub(super) fn auth_error(detail: &'static str) -> ProviderError {
    ProviderError::Authentication { detail }
}

pub(super) fn now() -> Result<u64, ProviderError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| auth_error("system clock is invalid"))
}

pub(super) fn random_value() -> Result<String, ProviderError> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| auth_error("random generation failed"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Credentials are private runtime data, separate from conversation context.
/// No Debug implementation: tokens must never enter diagnostics.
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Credentials {
    pub(super) issuer: String,
    pub(super) subject: String,
    pub(super) client_id: String,
    pub(super) ext_agent_host_id: String,
    pub(super) id_token: String,
    pub(super) access_token: String,
    pub(super) refresh_token: String,
    pub(super) scopes: Vec<String>,
    pub(super) expires_at: u64,
    #[serde(default)]
    pub(super) revocation_confirmed: bool,
}

impl Credentials {
    pub(super) fn signed_out(&self) -> bool {
        self.id_token.is_empty()
            && self.access_token.is_empty()
            && self.refresh_token.is_empty()
            && self.scopes.is_empty()
            && self.expires_at == 0
    }

    pub(super) fn validate_registration(&self) -> Result<(), ProviderError> {
        if self.issuer != ISSUER
            || self.subject.is_empty()
            || self.client_id.is_empty()
            || self.client_id == "dynamic_agent_client"
            || self.ext_agent_host_id.is_empty()
        {
            return Err(auth_error("credential registration is incomplete"));
        }
        Ok(())
    }

    pub(super) fn validate(&self) -> Result<(), ProviderError> {
        self.validate_registration()?;
        if self.issuer != ISSUER
            || self.subject.is_empty()
            || self.client_id.is_empty()
            || self.client_id == "dynamic_agent_client"
            || self.ext_agent_host_id.is_empty()
            || self.id_token.is_empty()
            || self.access_token.is_empty()
            || self.refresh_token.is_empty()
            || !self
                .scopes
                .iter()
                .any(|scope| scope == "chatgpt.tokens.use.direct")
            || !self.scopes.iter().any(|scope| scope == "offline_access")
            || !self.scopes.iter().any(|scope| scope == "resource.invoke")
        {
            return Err(auth_error(
                "credential record is incomplete or plan usage is not authorized",
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
pub(super) struct TokenResponse {
    pub(super) access_token: String,
    pub(super) refresh_token: String,
    #[serde(default)]
    pub(super) id_token: Option<String>,
    token_type: String,
    expires_in: u64,
    scope: String,
}

impl TokenResponse {
    pub(super) fn apply(self, credentials: &mut Credentials) -> Result<(), ProviderError> {
        if !self.token_type.eq_ignore_ascii_case("Bearer") || self.expires_in == 0 {
            return Err(auth_error("invalid token response"));
        }
        credentials.access_token = self.access_token;
        credentials.refresh_token = self.refresh_token;
        if let Some(id_token) = self.id_token {
            credentials.id_token = id_token;
        }
        credentials.expires_at = now()?
            .checked_add(self.expires_in)
            .ok_or_else(|| auth_error("invalid token expiration"))?;
        credentials.scopes = self.scope.split_whitespace().map(str::to_owned).collect();
        credentials.revocation_confirmed = false;
        credentials.validate()
    }
}

pub(super) async fn token_exchange(
    http: &reqwest::Client,
    endpoint: &str,
    fields: &[(&str, &str)],
) -> Result<TokenResponse, ProviderError> {
    let response = http.post(endpoint).form(fields).send().await?;
    if !response.status().is_success() {
        return Err(auth_error("token exchange rejected; sign in again"));
    }
    let body = transport::read_body_limited(super::ProviderId::Codex, response).await?;
    if body.len() as u64 > MAX_CREDENTIAL_BYTES {
        return Err(auth_error("token response exceeds byte bound"));
    }
    serde_json::from_str(&body).map_err(|_| auth_error("invalid token response"))
}

pub(super) async fn save(path: PathBuf, credentials: Credentials) -> Result<(), ProviderError> {
    tokio::task::spawn_blocking(move || {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or(ProviderError::CredentialStorage)?;
        // The caller supplies an existing private directory. NamedTempFile
        // atomically replaces the record, including on Windows.
        let mut file = tempfile::NamedTempFile::new_in(parent)
            .map_err(|_| ProviderError::CredentialStorage)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|_| ProviderError::CredentialStorage)?;
        }
        let bytes =
            serde_json::to_vec(&credentials).map_err(|_| ProviderError::CredentialStorage)?;
        if bytes.len() as u64 > MAX_CREDENTIAL_BYTES {
            return Err(ProviderError::CredentialStorage);
        }
        file.write_all(&bytes)
            .map_err(|_| ProviderError::CredentialStorage)?;
        file.as_file()
            .sync_all()
            .map_err(|_| ProviderError::CredentialStorage)?;
        file.persist(&path)
            .map_err(|_| ProviderError::CredentialStorage)?;
        #[cfg(unix)]
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| ProviderError::CredentialStorage)?;
        Ok(())
    })
    .await
    .map_err(|_| ProviderError::CredentialStorage)?
}
