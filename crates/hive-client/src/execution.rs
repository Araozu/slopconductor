use super::*;
use hive_protocol::execution::ArtifactResponse;
use sha2::{Digest, Sha256};

impl DaemonClient {
    pub async fn models(&self) -> Result<Vec<ModelResponse>, ClientError> {
        self.get_json(self.endpoint.join("/v1/models")?).await
    }

    pub async fn capabilities(
        &self,
    ) -> Result<hive_protocol::execution::CapabilitiesResponse, ClientError> {
        self.get_json(self.endpoint.join("/v1/capabilities")?).await
    }

    pub async fn message(&self, id: &str) -> Result<MessageResponse, ClientError> {
        self.get_json(id_url(&self.endpoint, "/v1/messages", id, None)?)
            .await
    }

    pub async fn model_requests(
        &self,
        turn: &str,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Page<hive_protocol::execution::ModelRequestResponse>, ClientError> {
        let mut url = id_url(&self.endpoint, "/v1/turns", turn, Some("requests"))?;
        append_page_query(&mut url, after, limit);
        self.get_json(url).await
    }

    pub async fn tool_invocations(
        &self,
        turn: &str,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Page<hive_protocol::execution::ToolInvocationResponse>, ClientError> {
        let mut url = id_url(&self.endpoint, "/v1/turns", turn, Some("tools"))?;
        append_page_query(&mut url, after, limit);
        self.get_json(url).await
    }

    pub async fn tool_invocation(
        &self,
        id: &str,
    ) -> Result<hive_protocol::execution::ToolInvocationResponse, ClientError> {
        self.get_json(id_url(&self.endpoint, "/v1/tools", id, None)?)
            .await
    }

    pub async fn artifact(
        &self,
        id: &str,
    ) -> Result<hive_protocol::execution::ArtifactResponse, ClientError> {
        self.get_json(id_url(&self.endpoint, "/v1/artifacts", id, None)?)
            .await
    }

    pub async fn artifact_content(&self, id: &str) -> Result<ArtifactStream, ClientError> {
        let metadata = self.artifact(id).await?;
        if metadata.id != id || metadata.sha256 != id || metadata.size_bytes > 64 * 1024 * 1024 {
            return Err(ClientError::InvalidResponse);
        }
        let pending = self
            .authorized(self.stream_http.get(id_url(
                &self.endpoint,
                "/v1/artifacts",
                id,
                Some("content"),
            )?))
            .send();
        let response = tokio::time::timeout(Duration::from_secs(10), pending)
            .await
            .map_err(|_| ClientError::StreamTimeout)??;
        let status = response.status();
        if !status.is_success() {
            let body = read_bounded(response).await?;
            return Err(api_error(status.as_u16(), &body, self.token.as_ref()));
        }
        Ok(ArtifactStream {
            metadata,
            response,
            received: 0,
            hash: Sha256::new(),
            finished: false,
        })
    }

    /// Upload raw image bytes as a content-addressed artifact for later
    /// attachment to user messages. Idempotent: same bytes yield the same id.
    pub async fn upload_image(
        &self,
        bytes: &[u8],
        media_type: &str,
    ) -> Result<ArtifactResponse, ClientError> {
        self.upload_artifact(bytes, media_type).await
    }

    /// Upload raw artifact bytes (currently image uploads only).
    pub async fn upload_artifact(
        &self,
        bytes: &[u8],
        media_type: &str,
    ) -> Result<ArtifactResponse, ClientError> {
        self.require_feature("image-input").await?;
        if !matches!(
            media_type,
            "image/png" | "image/jpeg" | "image/gif" | "image/webp"
        ) || bytes.is_empty()
            || bytes.len() > 20 * 1024 * 1024
        {
            return Err(ClientError::InvalidResponse);
        }
        self.health().await?;
        let url = self.endpoint.join("/v1/artifacts")?;
        let response = self
            .authorized(
                self.http
                    .post(self.routed_url(url)?)
                    .header(reqwest::header::CONTENT_TYPE, media_type)
                    .body(bytes.to_vec()),
            )
            .send()
            .await?;
        self.decode_json(response).await
    }
}
