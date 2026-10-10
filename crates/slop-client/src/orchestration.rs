use super::*;
use slop_protocol::orchestration::*;

impl DaemonClient {
    pub async fn create_task(
        &self,
        request: &CreateTaskRequest,
    ) -> Result<TaskReceipt, ClientError> {
        self.require_feature("tasks-runs").await?;
        self.post_json(self.endpoint.join("/v1/tasks")?, request)
            .await
    }
    pub async fn task(&self, id: &str) -> Result<TaskResponse, ClientError> {
        self.get_json(id_url(&self.endpoint, "/v1/tasks", id, None)?)
            .await
    }
    pub async fn run(&self, id: &str) -> Result<RunResponse, ClientError> {
        self.get_json(id_url(&self.endpoint, "/v1/runs", id, None)?)
            .await
    }
    pub async fn tasks(
        &self,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Page<TaskResponse>, ClientError> {
        let mut url = self.endpoint.join("/v1/tasks")?;
        append_page_query(&mut url, after, limit);
        self.get_json(url).await
    }
    pub async fn runs(
        &self,
        id: &str,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Page<RunResponse>, ClientError> {
        let mut url = id_url(&self.endpoint, "/v1/tasks", id, Some("runs"))?;
        append_page_query(&mut url, after, limit);
        self.get_json(url).await
    }
    pub async fn task_events(
        &self,
        id: &str,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Page<TaskEventResponse>, ClientError> {
        let mut url = id_url(&self.endpoint, "/v1/tasks", id, Some("events"))?;
        append_page_query(&mut url, after, limit);
        self.get_json(url).await
    }
    pub async fn control_run(
        &self,
        id: &str,
        action: &str,
        request: &slop_protocol::chat::TurnControlRequest,
    ) -> Result<CommandReceipt, ClientError> {
        if !matches!(action, "pause" | "resume" | "cancel") {
            return Err(ClientError::InvalidIdentifier);
        }
        self.require_feature("tasks-runs").await?;
        self.post_json(
            id_url(&self.endpoint, "/v1/runs", id, Some(action))?,
            request,
        )
        .await
    }
    pub async fn retry_run(
        &self,
        id: &str,
        request: &RetryRunRequest,
    ) -> Result<TaskReceipt, ClientError> {
        self.require_feature("tasks-runs").await?;
        self.post_json(
            id_url(&self.endpoint, "/v1/runs", id, Some("retry"))?,
            request,
        )
        .await
    }
    pub async fn preview_batch(
        &self,
        spec: &BatchSpec,
    ) -> Result<BatchPreviewResponse, ClientError> {
        self.require_feature("batch-matrices").await?;
        let body = serde_json::to_vec(spec).map_err(|_| ClientError::InvalidResponse)?;
        if body.len() > 2 * 1024 * 1024 {
            return Err(ClientError::RequestTooLarge);
        }
        let response = self
            .authorized(
                self.http
                    .post(self.endpoint.join("/v1/batches/preview")?)
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(body),
            )
            .send()
            .await?;
        self.decode_json(response).await
    }
    pub async fn create_batch(
        &self,
        request: &CreateBatchRequest,
    ) -> Result<BatchReceipt, ClientError> {
        self.require_feature("batch-matrices").await?;
        self.post_json(self.endpoint.join("/v1/batches")?, request)
            .await
    }
    pub async fn batch(&self, id: &str) -> Result<BatchResponse, ClientError> {
        self.get_json(id_url(&self.endpoint, "/v1/batches", id, None)?)
            .await
    }
    pub async fn batches(
        &self,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Page<BatchResponse>, ClientError> {
        let mut url = self.endpoint.join("/v1/batches")?;
        append_page_query(&mut url, after, limit);
        self.get_json(url).await
    }
    pub async fn batch_members(
        &self,
        id: &str,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Page<TaskResponse>, ClientError> {
        let mut url = id_url(&self.endpoint, "/v1/batches", id, Some("members"))?;
        append_page_query(&mut url, after, limit);
        self.get_json(url).await
    }
    pub async fn batch_results(
        &self,
        id: &str,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Page<BatchResultResponse>, ClientError> {
        let mut url = id_url(&self.endpoint, "/v1/batches", id, Some("results"))?;
        append_page_query(&mut url, after, limit);
        self.get_json(url).await
    }
    pub async fn retry_batch(
        &self,
        id: &str,
        request: &RetryBatchRequest,
    ) -> Result<BatchReceipt, ClientError> {
        self.require_feature("batch-matrices").await?;
        self.post_json(
            id_url(&self.endpoint, "/v1/batches", id, Some("retry"))?,
            request,
        )
        .await
    }
}

impl DaemonClient {
    pub async fn task_instruction(
        &self,
        id: &str,
        request: &SendMessageRequest,
    ) -> Result<CommandReceipt, ClientError> {
        self.require_feature("tasks-runs").await?;
        self.post_json(
            id_url(&self.endpoint, "/v1/tasks", id, Some("instructions"))?,
            request,
        )
        .await
    }
}
