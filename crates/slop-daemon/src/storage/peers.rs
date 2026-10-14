use super::*;
use slop_protocol::peers::{PeerGrantResponse, PeerResponse, PeerScope};

pub(super) fn migrate(tx: &Transaction<'_>) -> StoreResult<()> {
    // IF NOT EXISTS also permits fixtures restoring an older logical schema.
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS peer_grants (
        id TEXT PRIMARY KEY, label TEXT NOT NULL, scope TEXT NOT NULL,
        digest TEXT NOT NULL UNIQUE, revoked INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE IF NOT EXISTS peers (
        id TEXT PRIMARY KEY, origin TEXT NOT NULL, owner_node_id TEXT NOT NULL,
        digest TEXT NOT NULL, removed INTEGER NOT NULL DEFAULT 0);",
    )
    .map_err(|_| StoreError::Database)
}

fn grant_row(row: &rusqlite::Row<'_>, owner: &str) -> rusqlite::Result<PeerGrantResponse> {
    Ok(PeerGrantResponse {
        id: row.get(0)?,
        owner_node_id: owner.to_owned(),
        label: row.get(1)?,
        scope: if row.get::<_, String>(2)? == "control" {
            PeerScope::Control
        } else {
            PeerScope::Read
        },
        revoked: row.get(3)?,
    })
}
fn peer_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PeerResponse> {
    Ok(PeerResponse {
        id: row.get(0)?,
        origin: row.get(1)?,
        owner_node_id: row.get(2)?,
        removed: row.get(3)?,
    })
}
impl StoreClient {
    pub async fn peer_grants(&self) -> StoreResult<Vec<PeerGrantResponse>> {
        self.submit(|db| {
            let owner = query_node(db)?.node_id;
            let mut query = db
                .prepare("SELECT id,label,scope,revoked FROM peer_grants ORDER BY id")
                .map_err(|_| StoreError::Database)?;
            query
                .query_map([], |r| grant_row(r, &owner))
                .map_err(|_| StoreError::Database)?
                .collect::<rusqlite::Result<_>>()
                .map_err(|_| StoreError::Database)
        })
        .await
    }
    pub async fn put_peer_grant(
        &self,
        id: String,
        label: String,
        scope: PeerScope,
        digest: String,
    ) -> StoreResult<PeerGrantResponse> {
        self.submit(move |db| {
            let tx = db
                .unchecked_transaction()
                .map_err(|_| StoreError::Database)?;
            let scope_text = if scope == PeerScope::Control {
                "control"
            } else {
                "read"
            };
            let existing: Option<(String, String, String)> = tx
                .query_row(
                    "SELECT label,scope,digest FROM peer_grants WHERE id=?1",
                    [&id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()
                .map_err(|_| StoreError::Database)?;
            if let Some(old) = existing {
                if old != (label.clone(), scope_text.into(), digest.clone()) {
                    return Err(StoreError::Conflict);
                }
            } else {
                let count: i64 = tx
                    .query_row("SELECT COUNT(*) FROM peer_grants", [], |r| r.get(0))
                    .map_err(|_| StoreError::Database)?;
                if count >= 128 {
                    return Err(StoreError::Limit);
                }
                tx.execute(
                    "INSERT INTO peer_grants(id,label,scope,digest) VALUES(?1,?2,?3,?4)",
                    rusqlite::params![id, label, scope_text, digest],
                )
                .map_err(|_| StoreError::Conflict)?;
            }
            let owner = query_node(&tx)?.node_id;
            let result = tx
                .query_row(
                    "SELECT id,label,scope,revoked FROM peer_grants WHERE id=?1",
                    [&id],
                    |r| grant_row(r, &owner),
                )
                .map_err(|_| StoreError::Database)?;
            tx.commit().map_err(|_| StoreError::Database)?;
            Ok(result)
        })
        .await
    }
    pub async fn authenticate_peer(&self, digest: String) -> StoreResult<PeerGrantResponse> {
        self.submit(move |db| {
            let owner = query_node(db)?.node_id;
            db.query_row(
                "SELECT id,label,scope,revoked FROM peer_grants WHERE digest=?1 AND revoked=0",
                [digest],
                |r| grant_row(r, &owner),
            )
            .optional()
            .map_err(|_| StoreError::Database)?
            .ok_or(StoreError::NotFound)
        })
        .await
    }
    pub async fn revoke_peer_grant(&self, id: String) -> StoreResult<PeerGrantResponse> {
        self.submit(move |db| {
            db.execute("UPDATE peer_grants SET revoked=1 WHERE id=?1", [&id])
                .map_err(|_| StoreError::Database)?;
            let owner = query_node(db)?.node_id;
            db.query_row(
                "SELECT id,label,scope,revoked FROM peer_grants WHERE id=?1",
                [id],
                |r| grant_row(r, &owner),
            )
            .optional()
            .map_err(|_| StoreError::Database)?
            .ok_or(StoreError::NotFound)
        })
        .await
    }
    pub async fn peers(&self) -> StoreResult<Vec<PeerResponse>> {
        self.submit(|db| {
            let mut query = db
                .prepare("SELECT id,origin,owner_node_id,removed FROM peers ORDER BY id")
                .map_err(|_| StoreError::Database)?;
            query
                .query_map([], peer_row)
                .map_err(|_| StoreError::Database)?
                .collect::<rusqlite::Result<_>>()
                .map_err(|_| StoreError::Database)
        })
        .await
    }
    pub async fn peer(&self, id: String) -> StoreResult<(PeerResponse, String)> {
        self.submit(move |db| {
            db.query_row(
                "SELECT id,origin,owner_node_id,removed,digest FROM peers WHERE id=?1",
                [id],
                |r| Ok((peer_row(r)?, r.get(4)?)),
            )
            .optional()
            .map_err(|_| StoreError::Database)?
            .ok_or(StoreError::NotFound)
        })
        .await
    }
    pub async fn put_peer(&self, peer: PeerResponse, digest: String) -> StoreResult<PeerResponse> {
        self.submit(move |db| {
            let tx = db
                .unchecked_transaction()
                .map_err(|_| StoreError::Database)?;
            let old = tx
                .query_row(
                    "SELECT id,origin,owner_node_id,removed,digest FROM peers WHERE id=?1",
                    [&peer.id],
                    |r| Ok((peer_row(r)?, r.get::<_, String>(4)?)),
                )
                .optional()
                .map_err(|_| StoreError::Database)?;
            if let Some((old, old_digest)) = old {
                if old != peer || old_digest != digest {
                    return Err(StoreError::Conflict);
                }
            } else {
                let count: i64 = tx
                    .query_row("SELECT COUNT(*) FROM peers", [], |r| r.get(0))
                    .map_err(|_| StoreError::Database)?;
                if count >= 128 {
                    return Err(StoreError::Limit);
                }
                tx.execute(
                    "INSERT INTO peers(id,origin,owner_node_id,digest) VALUES(?1,?2,?3,?4)",
                    rusqlite::params![peer.id, peer.origin, peer.owner_node_id, digest],
                )
                .map_err(|_| StoreError::Database)?;
            }
            tx.commit().map_err(|_| StoreError::Database)?;
            Ok(peer)
        })
        .await
    }
    pub async fn remove_peer(&self, id: String) -> StoreResult<PeerResponse> {
        self.submit(move |db| {
            db.execute("UPDATE peers SET removed=1 WHERE id=?1", [&id])
                .map_err(|_| StoreError::Database)?;
            db.query_row(
                "SELECT id,origin,owner_node_id,removed FROM peers WHERE id=?1",
                [id],
                peer_row,
            )
            .optional()
            .map_err(|_| StoreError::Database)?
            .ok_or(StoreError::NotFound)
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn immutable_grants_and_removed_peers_survive_restart_without_resurrection() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = super::super::tests::open_test(&dir, None).await;
        let client = store.client();
        let grant = client
            .put_peer_grant(
                "work".into(),
                "Work".into(),
                PeerScope::Read,
                "digest".into(),
            )
            .await
            .unwrap();
        assert_eq!(
            client.authenticate_peer("digest".into()).await.unwrap(),
            grant
        );
        assert!(matches!(
            client
                .put_peer_grant(
                    "work".into(),
                    "Work".into(),
                    PeerScope::Control,
                    "digest".into()
                )
                .await,
            Err(StoreError::Conflict)
        ));
        assert!(
            client
                .revoke_peer_grant("work".into())
                .await
                .unwrap()
                .revoked
        );
        assert!(
            client
                .put_peer_grant(
                    "work".into(),
                    "Work".into(),
                    PeerScope::Read,
                    "digest".into()
                )
                .await
                .unwrap()
                .revoked
        );
        let peer = PeerResponse {
            id: "home".into(),
            origin: "https://home.example".into(),
            owner_node_id: "home-node".into(),
            removed: false,
        };
        client
            .put_peer(peer.clone(), "outbound-digest".into())
            .await
            .unwrap();
        client.remove_peer("home".into()).await.unwrap();
        assert!(matches!(
            client.put_peer(peer, "outbound-digest".into()).await,
            Err(StoreError::Conflict)
        ));
        store.shutdown().await.unwrap();
        let store = super::super::tests::open_test(&dir, None).await;
        let client = store.client();
        assert!(matches!(
            client.authenticate_peer("digest".into()).await,
            Err(StoreError::NotFound)
        ));
        assert!(client.peer("home".into()).await.unwrap().0.removed);
        store.shutdown().await.unwrap();
    }
}
