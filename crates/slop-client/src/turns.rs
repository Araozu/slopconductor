use super::*;

impl DaemonClient {
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
        request: &slop_protocol::chat::TurnControlRequest,
    ) -> Result<slop_protocol::chat::CommandReceipt, ClientError> {
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
        request: &slop_protocol::chat::TurnControlRequest,
    ) -> Result<slop_protocol::chat::CommandReceipt, ClientError> {
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
        slop_protocol::chat::Page<slop_protocol::chat::SteeringInstructionResponse>,
        ClientError,
    > {
        self.require_feature("execution-steering").await?;
        let mut url = session_url(&self.endpoint, session_id, Some("instructions"))?;
        append_page_query(&mut url, after, limit);
        self.get_json(url).await
    }
}
