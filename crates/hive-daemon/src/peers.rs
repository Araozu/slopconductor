//! Shared peer transport, private credentials, and bounded network admissions.
use crate::{
    auth,
    storage::{StoreClient, StoreError},
};
use hive_client::{ClientError, DaemonClient};
use hive_protocol::peers::{PeerResponse, RegisterPeerRequest};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, watch};

pub(crate) fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
pub(crate) fn token_digest(value: &str) -> Option<String> {
    (value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then(|| format!("{:x}", Sha256::digest(value.as_bytes())))
}
pub(crate) fn valid_origin(value: &str) -> bool {
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    let loopback = url.host_str().is_some_and(|host| {
        host.trim_matches(['[', ']'])
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
    });
    (url.scheme() == "https" || (url.scheme() == "http" && loopback))
        && url.host_str().is_some()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && value.len() <= 2048
}
pub struct Outbound {
    pub client: DaemonClient,
    pub peer: PeerResponse,
    pub digest: String,
}
pub struct PeerService {
    pub store: StoreClient,
    directory: PathBuf,
    pub changed: watch::Sender<()>,
    closing: AtomicBool,
    admin: Arc<tokio::sync::Mutex<()>>,
    clients: Mutex<HashMap<String, Arc<Outbound>>>,
    gates: Mutex<HashMap<String, Arc<Semaphore>>>,
    global: Arc<Semaphore>,
    streams: Arc<Semaphore>,
}
impl PeerService {
    pub fn new(store: StoreClient, data_dir: &Path) -> Self {
        Self {
            store,
            directory: data_dir.join("credentials"),
            changed: watch::channel(()).0,
            closing: AtomicBool::new(false),
            admin: Arc::new(tokio::sync::Mutex::new(())),
            clients: Mutex::new(HashMap::new()),
            gates: Mutex::new(HashMap::new()),
            global: Arc::new(Semaphore::new(32)),
            streams: Arc::new(Semaphore::new(16)),
        }
    }
    pub fn shutdown(&self) {
        self.closing.store(true, Ordering::Release);
        self.changed.send_replace(());
    }
    pub async fn drain(&self) {
        let _guard = self.admin.lock().await;
    }
    pub fn is_closing(&self) -> bool {
        self.closing.load(Ordering::Acquire)
    }
    pub fn notify(&self) {
        self.changed.send_replace(());
    }
    pub fn admit(
        &self,
        key: String,
        stream: bool,
    ) -> Result<Vec<OwnedSemaphorePermit>, &'static str> {
        if self.is_closing() {
            return Err("shutting_down");
        }
        let mut gates = self.gates.lock().map_err(|_| "peer_unavailable")?;
        // At most 128 inbound grants and 128 outbound registrations, two gates each.
        if !gates.contains_key(&key) && gates.len() >= 512 {
            return Err("peer_limit");
        }
        let gate = gates
            .entry(key)
            .or_insert_with(|| Arc::new(Semaphore::new(if stream { 4 } else { 8 })))
            .clone();
        let mut permits = vec![
            self.global
                .clone()
                .try_acquire_owned()
                .map_err(|_| "peer_limit")?,
            gate.try_acquire_owned().map_err(|_| "peer_limit")?,
        ];
        if stream {
            permits.push(
                self.streams
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| "peer_limit")?,
            );
        }
        Ok(permits)
    }
    pub async fn register(
        &self,
        id: String,
        request: RegisterPeerRequest,
    ) -> Result<PeerResponse, StoreError> {
        let guard = self.admin.clone().lock_owned().await;
        let digest = token_digest(&request.token).ok_or(StoreError::Invalid)?;
        let peer = PeerResponse {
            id: id.clone(),
            origin: request.origin,
            owner_node_id: request.owner_node_id,
            removed: false,
        };
        match self.store.peer(id.clone()).await {
            Ok((old, old_digest)) => {
                if old != peer || old_digest != digest {
                    return Err(StoreError::Conflict);
                }
            }
            Err(StoreError::NotFound) => {}
            Err(error) => return Err(error),
        }
        let path = self.directory.join(format!("peer-{digest}"));
        let secret = format!("{}\n", request.token).into_bytes();
        let _guard = tokio::task::spawn_blocking(move || {
            auth::write_private_file(&path, &secret)?;
            Ok::<_, auth::AuthError>(guard)
        })
        .await
        .map_err(|_| StoreError::Unavailable)?
        .map_err(|_| StoreError::Configuration)?;
        let result = self.store.put_peer(peer, digest).await?;
        self.notify();
        Ok(result)
    }
    pub async fn outbound(&self, id: &str) -> Result<Arc<Outbound>, ClientError> {
        let (peer, digest) =
            self.store
                .peer(id.to_owned())
                .await
                .map_err(|_| ClientError::Api {
                    status: 404,
                    code: "peer_not_found".into(),
                    message: "The peer registration is unavailable.".into(),
                })?;
        if peer.removed {
            return Err(ClientError::Api {
                status: 404,
                code: "peer_removed".into(),
                message: "The peer registration was removed.".into(),
            });
        }
        if let Some(cached) = self
            .clients
            .lock()
            .map_err(|_| ClientError::InvalidResponse)?
            .get(id)
            .filter(|c| c.peer == peer && c.digest == digest)
            .cloned()
        {
            return Ok(cached);
        }
        let path = self.directory.join(format!("peer-{digest}"));
        let origin = peer.origin.clone();
        let owner_node_id = peer.owner_node_id.clone();
        let client = tokio::task::spawn_blocking(move || {
            let file = auth::open_private_file(&path).map_err(|_| ClientError::InvalidTokenFile)?;
            drop(file);
            DaemonClient::new_with_token_file(&origin, &path)?.with_expected_owner(&owner_node_id)
        })
        .await
        .map_err(|_| ClientError::InvalidResponse)??;
        let outbound = Arc::new(Outbound {
            client,
            peer,
            digest,
        });
        self.clients
            .lock()
            .map_err(|_| ClientError::InvalidResponse)?
            .insert(id.to_owned(), outbound.clone());
        Ok(outbound)
    }
    pub async fn verify(
        &self,
        outbound: &Outbound,
    ) -> Result<hive_protocol::NodeResponse, ClientError> {
        let node = outbound.client.node().await?;
        if node.node_id != outbound.peer.owner_node_id {
            return Err(ClientError::Api {
                status: 409,
                code: "peer_identity_mismatch".into(),
                message: "The owner node identity changed; register the intended owner explicitly."
                    .into(),
            });
        }
        Ok(node)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn peer_credentials_require_tls_except_literal_loopback_fixtures() {
        for origin in [
            "https://home.tailnet.ts.net",
            "https://home.tailnet.ts.net:8443",
            "http://127.0.0.1:7332",
            "http://[::1]:7332",
        ] {
            assert!(valid_origin(origin), "{origin}");
        }
        for origin in [
            "http://home.tailnet.ts.net",
            "http://localhost:7332",
            "http://100.64.0.1:7332",
            "https://user:secret@home.test",
            "https://home.test/v1",
            "https://home.test/?token=secret",
            "https://home.test/#secret",
            "file:///tmp/peer",
            "https:///",
            "https://home.test/../v1",
        ] {
            assert!(!valid_origin(origin), "{origin}");
        }
    }
}
