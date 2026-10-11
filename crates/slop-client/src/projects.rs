use super::*;

impl DaemonClient {
    pub async fn register_project(
        &self,
        request: &slop_protocol::projects::RegisterProjectRequest,
    ) -> Result<slop_protocol::projects::ProjectResponse, ClientError> {
        self.require_feature("managed-workspaces").await?;
        self.post_json(self.endpoint.join("/v1/projects")?, request)
            .await
    }

    pub async fn projects(
        &self,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Page<slop_protocol::projects::ProjectResponse>, ClientError> {
        self.get_json(query_url(
            &self.endpoint,
            "/v1/projects",
            after,
            limit,
            None,
        )?)
        .await
    }

    pub async fn project(
        &self,
        id: &str,
    ) -> Result<slop_protocol::projects::ProjectResponse, ClientError> {
        self.get_json(id_url(&self.endpoint, "/v1/projects", id, None)?)
            .await
    }

    pub async fn project_workspaces(
        &self,
        id: &str,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Page<slop_protocol::projects::WorkspaceResponse>, ClientError> {
        let mut url = id_url(&self.endpoint, "/v1/projects", id, Some("workspaces"))?;
        append_page_query(&mut url, after, limit);
        self.get_json(url).await
    }

    pub async fn project_events(
        &self,
        id: &str,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Page<slop_protocol::projects::ProjectEventResponse>, ClientError> {
        let mut url = id_url(&self.endpoint, "/v1/projects", id, Some("events"))?;
        append_page_query(&mut url, after, limit);
        self.get_json(url).await
    }

    pub async fn workspace(
        &self,
        id: &str,
    ) -> Result<slop_protocol::projects::WorkspaceResponse, ClientError> {
        self.get_json(id_url(&self.endpoint, "/v1/workspaces", id, None)?)
            .await
    }

    pub async fn workspace_diff(
        &self,
        id: &str,
    ) -> Result<slop_protocol::projects::WorkspaceDiffResponse, ClientError> {
        self.get_json(id_url(&self.endpoint, "/v1/workspaces", id, Some("diff"))?)
            .await
    }

    pub async fn remove_workspace(
        &self,
        id: &str,
        request: &slop_protocol::projects::RemoveWorkspaceRequest,
    ) -> Result<slop_protocol::projects::WorkspaceResponse, ClientError> {
        self.require_feature("managed-workspaces").await?;
        self.post_json(
            id_url(&self.endpoint, "/v1/workspaces", id, Some("remove"))?,
            request,
        )
        .await
    }
}
