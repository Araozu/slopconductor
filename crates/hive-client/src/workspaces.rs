use super::*;

impl DaemonClient {
    pub async fn browse_directory(
        &self,
        path: Option<&str>,
        after: Option<&str>,
        limit: Option<u32>,
    ) -> Result<hive_protocol::filesystem::DirectoryResponse, ClientError> {
        self.require_feature("filesystem-navigation").await?;
        let mut url = self.endpoint.join("/v1/filesystem")?;
        {
            let mut query = url.query_pairs_mut();
            if let Some(path) = path {
                query.append_pair("path", path);
            }
            if let Some(after) = after {
                query.append_pair("after", after);
            }
            if let Some(limit) = limit {
                query.append_pair("limit", &limit.to_string());
            }
        }
        self.get_json(url).await
    }

    pub async fn create_directory(
        &self,
        request: &hive_protocol::filesystem::CreateDirectoryRequest,
    ) -> Result<hive_protocol::filesystem::DirectoryResponse, ClientError> {
        self.require_feature("filesystem-navigation").await?;
        self.post_json(self.endpoint.join("/v1/filesystem/directories")?, request)
            .await
    }

    pub async fn open_workspace(
        &self,
        request: &hive_protocol::workspaces::OpenWorkspaceRequest,
    ) -> Result<hive_protocol::workspaces::WorkspaceResponse, ClientError> {
        self.require_feature("unified-workspaces").await?;
        self.post_json(self.endpoint.join("/v1/workspaces")?, request)
            .await
    }

    pub async fn create_workspace(
        &self,
        request: &hive_protocol::workspaces::CreateWorkspaceRequest,
    ) -> Result<hive_protocol::workspaces::WorkspaceResponse, ClientError> {
        self.require_feature("unified-workspaces").await?;
        self.post_json(self.endpoint.join("/v1/workspaces/create")?, request)
            .await
    }

    pub async fn workspaces(
        &self,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Page<hive_protocol::workspaces::WorkspaceResponse>, ClientError> {
        self.require_feature("unified-workspaces").await?;
        self.get_json(query_url(
            &self.endpoint,
            "/v1/workspaces",
            after,
            limit,
            None,
        )?)
        .await
    }

    pub async fn workspace(
        &self,
        id: &str,
    ) -> Result<hive_protocol::workspaces::WorkspaceResponse, ClientError> {
        self.require_feature("unified-workspaces").await?;
        self.get_json(id_url(&self.endpoint, "/v1/workspaces", id, None)?)
            .await
    }

    pub async fn rename_workspace(
        &self,
        id: &str,
        request: &hive_protocol::workspaces::RenameWorkspaceRequest,
    ) -> Result<hive_protocol::workspaces::WorkspaceResponse, ClientError> {
        self.require_feature("unified-workspaces").await?;
        self.post_json(
            id_url(&self.endpoint, "/v1/workspaces", id, Some("rename"))?,
            request,
        )
        .await
    }

    pub async fn remove_workspace(
        &self,
        id: &str,
        request: &hive_protocol::workspaces::RemoveWorkspaceRequest,
    ) -> Result<hive_protocol::workspaces::WorkspaceResponse, ClientError> {
        self.require_feature("unified-workspaces").await?;
        self.post_json(
            id_url(&self.endpoint, "/v1/workspaces", id, Some("remove"))?,
            request,
        )
        .await
    }

    pub async fn workspace_events(
        &self,
        id: &str,
        after: Option<u64>,
        limit: Option<u32>,
    ) -> Result<Page<hive_protocol::workspaces::WorkspaceEventResponse>, ClientError> {
        self.require_feature("unified-workspaces").await?;
        let mut url = id_url(&self.endpoint, "/v1/workspaces", id, Some("events"))?;
        append_page_query(&mut url, after, limit);
        self.get_json(url).await
    }

    pub async fn create_worktree(
        &self,
        id: &str,
        request: &hive_protocol::workspaces::CreateWorktreeRequest,
    ) -> Result<hive_protocol::workspaces::WorkspaceResponse, ClientError> {
        self.require_feature("unified-workspaces").await?;
        self.post_json(
            id_url(&self.endpoint, "/v1/workspaces", id, Some("worktrees"))?,
            request,
        )
        .await
    }

    pub async fn workspace_diff(
        &self,
        id: &str,
    ) -> Result<hive_protocol::workspaces::WorkspaceDiffResponse, ClientError> {
        self.require_feature("unified-workspaces").await?;
        self.get_json(id_url(&self.endpoint, "/v1/workspaces", id, Some("diff"))?)
            .await
    }
}
