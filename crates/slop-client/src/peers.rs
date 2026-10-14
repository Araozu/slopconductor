use super::*;
use slop_protocol::peers::*;

impl DaemonClient {
    /// Route ordinary session/turn operations through the selected local peer.
    /// The local credential is sent only to the local daemon, never to its peer.
    pub fn via_peer(mut self, peer: &str) -> Result<Self, ClientError> {
        id_url(&self.endpoint, "/v1/peers", peer, None)?;
        self.peer = Some(peer.to_owned());
        Ok(self)
    }

    /// Bind authenticated owner requests at the destination as well as during
    /// identity probing, including across a restart between two HTTP requests.
    pub fn with_expected_owner(mut self, owner: &str) -> Result<Self, ClientError> {
        id_url(&self.endpoint, "/v1/node", owner, None)?;
        self.expected_owner =
            Some(HeaderValue::from_str(owner).map_err(|_| ClientError::InvalidIdentifier)?);
        Ok(self)
    }

    pub(super) fn routed_url(&self, mut url: Url) -> Result<Url, ClientError> {
        if let Some(peer) = &self.peer {
            let suffix = url
                .path()
                .strip_prefix("/v1/")
                .ok_or(ClientError::InvalidOrigin)?;
            url.set_path(&format!("/v1/peers/{peer}/{suffix}"));
        }
        Ok(url)
    }

    pub async fn peer_grants(&self) -> Result<Vec<PeerGrantResponse>, ClientError> {
        self.get_json(self.endpoint.join("/v1/peer-grants")?).await
    }
    pub async fn create_peer_grant(
        &self,
        id: &str,
        request: &CreatePeerGrantRequest,
    ) -> Result<PeerGrantResponse, ClientError> {
        self.mutate_json(
            reqwest::Method::PUT,
            id_url(&self.endpoint, "/v1/peer-grants", id, None)?,
            request,
        )
        .await
    }
    pub async fn revoke_peer_grant(&self, id: &str) -> Result<PeerGrantResponse, ClientError> {
        self.post_json(
            id_url(&self.endpoint, "/v1/peer-grants", id, Some("revoke"))?,
            &serde_json::json!({}),
        )
        .await
    }
    pub async fn peers(&self) -> Result<Vec<PeerResponse>, ClientError> {
        self.get_json(self.endpoint.join("/v1/peers")?).await
    }
    pub async fn register_peer(
        &self,
        id: &str,
        request: &RegisterPeerRequest,
    ) -> Result<PeerResponse, ClientError> {
        self.mutate_json(
            reqwest::Method::PUT,
            id_url(&self.endpoint, "/v1/peers", id, None)?,
            request,
        )
        .await
    }
    pub async fn remove_peer(&self, id: &str) -> Result<PeerResponse, ClientError> {
        self.mutate_json(
            reqwest::Method::DELETE,
            id_url(&self.endpoint, "/v1/peers", id, None)?,
            &serde_json::json!({}),
        )
        .await
    }
    pub async fn probe_peer(&self, id: &str) -> Result<PeerProbeResponse, ClientError> {
        self.get_json(id_url(&self.endpoint, "/v1/peers", id, Some("probe"))?)
            .await
    }
}
