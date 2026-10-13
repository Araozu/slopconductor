use super::*;

impl DaemonClient {
    pub async fn update_session_inbox(
        &self,
        session_id: &str,
        request: &UpdateSessionInboxRequest,
    ) -> Result<CommandReceipt, ClientError> {
        self.require_feature("durable-inbox").await?;
        self.post_json(
            session_url(&self.endpoint, session_id, Some("inbox"))?,
            request,
        )
        .await
    }

    pub async fn create_session(
        &self,
        request: &CreateSessionRequest,
    ) -> Result<CommandReceipt, ClientError> {
        if request.execution.is_some() {
            self.require_feature("tools").await?;
        }
        if request.project.is_some() {
            self.require_feature("managed-workspaces").await?;
        }
        if request.settings.is_some() {
            self.require_feature("per-turn-settings").await?;
        }
        if request
            .settings
            .as_ref()
            .is_some_and(|s| s.full_context.is_some())
        {
            self.require_feature("full-context").await?;
        }
        self.require_feature("persistent-conversations").await?;
        self.post_json(self.endpoint.join("/v1/sessions")?, request)
            .await
    }

    pub async fn list_sessions(
        &self,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Page<SessionResponse>, ClientError> {
        self.get_json(query_url(
            &self.endpoint,
            "/v1/sessions",
            after,
            limit,
            None,
        )?)
        .await
    }

    pub async fn session(&self, session_id: &str) -> Result<SessionResponse, ClientError> {
        self.get_json(session_url(&self.endpoint, session_id, None)?)
            .await
    }

    pub async fn history(
        &self,
        session_id: &str,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Page<MessageResponse>, ClientError> {
        let mut url = session_url(&self.endpoint, session_id, Some("messages"))?;
        append_page_query(&mut url, after, limit);
        self.get_json(url).await
    }

    pub async fn send_message(
        &self,
        session_id: &str,
        request: &SendMessageRequest,
    ) -> Result<CommandReceipt, ClientError> {
        if request.model.is_some() || request.settings.is_some() {
            self.require_feature("per-turn-settings").await?;
        }
        if request
            .settings
            .as_ref()
            .is_some_and(|s| s.full_context.is_some())
        {
            self.require_feature("full-context").await?;
        }
        if request.delivery.is_some() {
            self.require_feature("execution-steering").await?;
        }
        self.post_json(
            session_url(&self.endpoint, session_id, Some("messages"))?,
            request,
        )
        .await
    }

    pub async fn events(
        &self,
        session_id: &str,
        after: u64,
        follow: bool,
    ) -> Result<EventStream, ClientError> {
        self.health().await?;
        let mut url = session_url(&self.endpoint, session_id, Some("events"))?;
        url.query_pairs_mut()
            .append_pair("after", &after.to_string())
            .append_pair("follow", if follow { "true" } else { "false" });
        let mut request = self.stream_http.get(url);
        if let Some(token) = &self.token {
            request = request.header(AUTHORIZATION, token.clone());
        }
        let response = request.send().await?;
        let status = response.status();
        if !status.is_success() {
            let body = read_bounded(response).await?;
            return Err(api_error(status.as_u16(), &body, self.token.as_ref()));
        }
        Ok(EventStream {
            response,
            buffer: Vec::new(),
            finished: false,
        })
    }
}
