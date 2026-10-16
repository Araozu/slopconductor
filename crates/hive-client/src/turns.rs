use super::*;

impl DaemonClient {
    pub async fn update_queued_message(
        &self,
        session: &str,
        message: &str,
        request: &hive_protocol::chat::UpdateQueuedMessageRequest,
    ) -> Result<CommandReceipt, ClientError> {
        self.require_feature("queue-editing").await?;
        let base = session_url(&self.endpoint, session, Some("queue"))?;
        self.mutate_json(
            reqwest::Method::PATCH,
            id_url(&self.endpoint, base.path(), message, None)?,
            request,
        )
        .await
    }

    pub async fn cancel_queued_message(
        &self,
        session: &str,
        message: &str,
        request: &CancelTurnRequest,
    ) -> Result<CommandReceipt, ClientError> {
        self.require_feature("queue-editing").await?;
        let base = session_url(&self.endpoint, session, Some("queue"))?;
        self.mutate_json(
            reqwest::Method::DELETE,
            id_url(&self.endpoint, base.path(), message, None)?,
            request,
        )
        .await
    }

    pub async fn queued_messages(
        &self,
        session_id: &str,
    ) -> Result<Vec<hive_protocol::chat::QueuedMessageResponse>, ClientError> {
        self.require_feature("message-queue").await?;
        self.get_json(session_url(&self.endpoint, session_id, Some("queue"))?)
            .await
    }

    pub async fn interrupt_session(
        &self,
        session_id: &str,
        request: &hive_protocol::chat::InterruptSessionRequest,
    ) -> Result<CommandReceipt, ClientError> {
        self.require_feature("message-queue").await?;
        self.post_json(
            session_url(&self.endpoint, session_id, Some("interrupt"))?,
            request,
        )
        .await
    }

    pub async fn turn(&self, turn_id: &str) -> Result<TurnResponse, ClientError> {
        self.get_json(id_url(&self.endpoint, "/v1/turns", turn_id, None)?)
            .await
    }

    pub async fn cancel_turn(
        &self,
        turn_id: &str,
        request: &CancelTurnRequest,
    ) -> Result<CommandReceipt, ClientError> {
        self.post_json(
            id_url(&self.endpoint, "/v1/turns", turn_id, Some("cancel"))?,
            request,
        )
        .await
    }

    pub async fn pause_turn(
        &self,
        turn_id: &str,
        request: &hive_protocol::chat::TurnControlRequest,
    ) -> Result<hive_protocol::chat::CommandReceipt, ClientError> {
        self.require_feature("turn-pause-resume").await?;
        self.post_json(
            id_url(&self.endpoint, "/v1/turns", turn_id, Some("pause"))?,
            request,
        )
        .await
    }

    pub async fn resume_turn(
        &self,
        turn_id: &str,
        request: &hive_protocol::chat::TurnControlRequest,
    ) -> Result<hive_protocol::chat::CommandReceipt, ClientError> {
        self.require_feature("turn-pause-resume").await?;
        self.post_json(
            id_url(&self.endpoint, "/v1/turns", turn_id, Some("resume"))?,
            request,
        )
        .await
    }

    pub async fn steering_instructions(
        &self,
        session_id: &str,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> Result<
        hive_protocol::chat::Page<hive_protocol::chat::SteeringInstructionResponse>,
        ClientError,
    > {
        self.require_feature("execution-steering").await?;
        let mut url = session_url(&self.endpoint, session_id, Some("instructions"))?;
        append_page_query(&mut url, after, limit);
        self.get_json(url).await
    }
}
