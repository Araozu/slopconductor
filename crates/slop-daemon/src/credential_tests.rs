use super::*;

#[tokio::test]
async fn selection_restores_mode_and_account_together_and_rejects_unknown_accounts() {
    let directory = tempfile::tempdir().unwrap();
    let credentials = directory.path().join("credentials");
    create_private_dir(&credentials).unwrap();
    for id in ["default", "work"] {
        let record = serde_json::json!({
            "issuer":"https://auth.openai.com", "subject":id,
            "client_id":format!("oaiapp_{id}"), "ext_agent_host_id":"host",
            "id_token":"", "access_token":"", "refresh_token":"",
            "scopes":[], "expires_at":0,
        });
        write_private_file(
            &account_path(&credentials, id),
            &serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
    }
    let selection_path = credentials.join("codex-auth-mode");
    write_codex_selection(&selection_path, None, Some("work")).unwrap();
    let (owner, _) = ProviderCredentials::load(directory.path(), "host".into(), vec![])
        .await
        .unwrap();
    let state = owner.state.lock().await;
    assert_eq!(state.active_account.as_deref(), Some("work"));
    assert!(state.active_codex_auth.is_none());
    drop(state);
    assert!(owner.provider_clients(None).await.unwrap().is_empty());
    write_codex_selection(&selection_path, Some("chatgpt"), Some("unknown")).unwrap();
    assert!(matches!(
        ProviderCredentials::load(directory.path(), "host".into(), vec![]).await,
        Err(CredentialError::Storage)
    ));
}

#[tokio::test]
async fn bootstrap_is_private_and_saved_keys_override_environment() {
    let directory = tempfile::tempdir().unwrap();
    let (owner, key) = ProviderCredentials::load(
        directory.path(),
        "host-1".into(),
        vec![("opencode-go".into(), "initial-key".into())],
    )
    .await
    .unwrap();
    assert_eq!(key.as_deref(), Some("initial-key"));
    assert!(owner.statuses().await[0].api_key_configured);
    let path = directory.path().join("credentials/opencode-go-api-key");
    write_private_file(&path, b"replacement-key").unwrap();
    let (_, key) = ProviderCredentials::load(
        directory.path(),
        "host-1".into(),
        vec![("opencode-go".into(), "stale-env-key".into())],
    )
    .await
    .unwrap();
    assert_eq!(key.as_deref(), Some("replacement-key"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    owner.shutdown().await;
}

#[tokio::test]
async fn duplicate_login_reuses_callback_and_shutdown_cancels_it() {
    let directory = tempfile::tempdir().unwrap();
    let (owner, _) = ProviderCredentials::load(directory.path(), "stable-host".into(), vec![])
        .await
        .unwrap();
    let first = owner.start_login("login-1".into(), None).await.unwrap();
    let repeated = owner.start_login("login-1".into(), None).await.unwrap();
    assert_eq!(first.authorization_url, repeated.authorization_url);
    assert!(
        first
            .authorization_url
            .contains("ext_agent_host_id=stable-host")
    );
    assert!(matches!(
        owner.start_login("login-2".into(), None).await,
        Err(CredentialError::LoginPending)
    ));
    tokio::time::timeout(std::time::Duration::from_secs(2), owner.shutdown())
        .await
        .unwrap();
    assert_eq!(
        owner.login_status("login-1").await.unwrap().status,
        "failed"
    );
    assert!(
        !directory
            .path()
            .join("credentials/codex-chatgpt.json")
            .exists()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn insecure_or_symlink_credentials_fail_closed() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let directory = tempfile::tempdir().unwrap();
    let credentials = directory.path().join("credentials");
    create_private_dir(&credentials).unwrap();
    let path = credentials.join("opencode-go-api-key");
    std::fs::write(&path, "key").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        ProviderCredentials::load(directory.path(), "host".into(), vec![])
            .await
            .is_err()
    );
    assert!(write_private_file(&path, b"new-key").is_err());
    std::fs::remove_file(&path).unwrap();
    symlink(directory.path().join("missing"), &path).unwrap();
    assert!(
        ProviderCredentials::load(directory.path(), "host".into(), vec![])
            .await
            .is_err()
    );
    assert!(write_private_file(&path, b"new-key").is_err());
}
