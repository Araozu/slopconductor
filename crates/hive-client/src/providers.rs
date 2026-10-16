use super::*;

impl DaemonClient {
    pub async fn providers(&self) -> Result<Vec<ProviderStatus>, ClientError> {
        self.get_json(self.endpoint.join(hive_protocol::PROVIDERS_PATH)?)
            .await
    }

    /// Idempotent replacement; transport failure may leave the key installed.
    pub async fn set_provider_api_key(
        &self,
        provider: &str,
        request: &SetApiKeyRequest,
    ) -> Result<ProviderStatus, ClientError> {
        self.mutate_json(
            reqwest::Method::PUT,
            id_url(
                &self.endpoint,
                hive_protocol::PROVIDERS_PATH,
                provider,
                Some("api-key"),
            )?,
            request,
        )
        .await
    }

    pub async fn start_codex_login(
        &self,
        request: &StartLoginRequest,
    ) -> Result<LoginResponse, ClientError> {
        if request.headless {
            self.require_feature("provider-headless-login").await?;
        }
        if request.account_id.is_some() {
            self.require_feature("provider-accounts").await?;
        }
        self.post_json(self.endpoint.join("/v1/providers/codex/login")?, request)
            .await
    }

    pub async fn codex_login_status(&self, login_id: &str) -> Result<LoginStatus, ClientError> {
        self.get_json(id_url(
            &self.endpoint,
            "/v1/providers/codex/login",
            login_id,
            None,
        )?)
        .await
    }

    /// Submit once; an uncertain token exchange must not be replayed.
    pub async fn submit_codex_login_callback(
        &self,
        login_id: &str,
        request: &LoginCallbackRequest,
    ) -> Result<LoginStatus, ClientError> {
        self.require_feature("provider-headless-login").await?;
        self.post_json(
            id_url(
                &self.endpoint,
                "/v1/providers/codex/login",
                login_id,
                Some("callback"),
            )?,
            request,
        )
        .await
    }

    pub async fn provider_catalog(&self, provider: &str) -> Result<ProviderCatalog, ClientError> {
        self.require_feature("provider-discovery").await?;
        self.get_json(id_url(
            &self.endpoint,
            hive_protocol::PROVIDERS_PATH,
            provider,
            Some("models"),
        )?)
        .await
    }

    pub async fn codex_accounts(&self) -> Result<Vec<ProviderAccount>, ClientError> {
        self.require_feature("provider-accounts").await?;
        self.get_json(self.endpoint.join("/v1/providers/codex/accounts")?)
            .await
    }

    pub async fn select_codex_auth(
        &self,
        request: &SelectAuthRequest,
    ) -> Result<ProviderStatus, ClientError> {
        self.require_feature("provider-accounts").await?;
        self.mutate_json(
            reqwest::Method::PUT,
            self.endpoint.join("/v1/providers/codex/auth")?,
            request,
        )
        .await
    }

    pub async fn sign_out_codex_account(
        &self,
        account_id: &str,
    ) -> Result<SignOutResponse, ClientError> {
        self.require_feature("provider-accounts").await?;
        self.post_json(
            id_url(
                &self.endpoint,
                "/v1/providers/codex/accounts",
                account_id,
                Some("logout"),
            )?,
            &serde_json::json!({}),
        )
        .await
    }

    pub async fn remove_provider_api_key(
        &self,
        provider: &str,
    ) -> Result<ProviderStatus, ClientError> {
        self.require_feature("provider-accounts").await?;
        self.mutate_json(
            reqwest::Method::DELETE,
            id_url(
                &self.endpoint,
                hive_protocol::PROVIDERS_PATH,
                provider,
                Some("api-key"),
            )?,
            &serde_json::json!({}),
        )
        .await
    }
}
