use super::super::test_http::{Reply, Server};
use super::credentials::now;
use super::identity::verify_identity;
use super::*;

#[tokio::test]
async fn sign_out_clears_tokens_keeps_registration_and_reports_revocation() {
    for confirmed in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("account.json");
        let server = Server::new(vec![]).await;
        server.enqueue(vec![
            Reply::json(
                serde_json::json!({"revocation_endpoint":format!("{}/revoke", server.url)}),
            ),
            Reply {
                status: if confirmed {
                    reqwest::StatusCode::OK
                } else {
                    reqwest::StatusCode::SERVICE_UNAVAILABLE
                },
                content_type: "application/json",
                body: String::new(),
                location: None,
            },
        ]);
        let mut connection = fixture_connection(path.clone(), u64::MAX);
        connection.discovery_endpoint = format!("{}/discovery", server.url);
        let scope = connection.continuation_identity().await;
        assert_eq!(connection.sign_out().await.unwrap(), confirmed);
        assert!(!connection.authenticated().await);
        assert!(connection.access_token().await.is_err());
        assert_eq!(connection.sign_out().await.unwrap(), confirmed);
        let saved = std::fs::read_to_string(&path).unwrap();
        for token in [
            "fixture-id-token",
            "fixture-access-token",
            "fixture-refresh-token",
        ] {
            assert!(!saved.contains(token));
        }
        let restored = ChatGptConnection::load(&path).await.unwrap();
        assert_eq!(scope, restored.continuation_identity().await);
        let login = restored.begin_login().await.unwrap();
        let url = reqwest::Url::parse(login.authorization_url()).unwrap();
        assert!(
            url.query_pairs()
                .any(|(key, value)| key == "client_id" && value == "oaiapp_fixture")
        );
        server.inspect(|requests| {
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[1].path, "/revoke");
            let values: std::collections::HashMap<_, _> =
                reqwest::Url::parse(&format!("http://localhost/?{}", requests[1].body))
                    .unwrap()
                    .query_pairs()
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect();
            assert_eq!(values["token"], "fixture-refresh-token");
            assert_eq!(values["client_id"], "oaiapp_fixture");
        });
    }
}

#[tokio::test]
async fn sign_out_never_posts_tokens_to_untrusted_discovery_destination() {
    let directory = tempfile::tempdir().unwrap();
    let server = Server::new(vec![Reply::json(
        serde_json::json!({"revocation_endpoint":"https://example.com/revoke"}),
    )])
    .await;
    let parent = directory.path().join("missing");
    let path = parent.join("account.json");
    let mut connection = fixture_connection(path.clone(), u64::MAX);
    connection.discovery_endpoint = format!("{}/discovery", server.url);
    assert!(matches!(
        connection.sign_out().await,
        Err(ProviderError::CredentialStorage)
    ));
    assert!(!connection.authenticated().await);
    std::fs::create_dir(&parent).unwrap();
    assert!(!connection.sign_out().await.unwrap());
    assert!(
        !ChatGptConnection::load(path)
            .await
            .unwrap()
            .authenticated()
            .await
    );
    server.inspect(|requests| assert_eq!(requests.len(), 1));
}

#[tokio::test]
async fn login_uses_pkce_host_identity_and_strict_callback_binding() {
    let login = ChatGptLogin::begin("persistent-host").await.unwrap();
    let url = reqwest::Url::parse(login.authorization_url()).unwrap();
    let fields: std::collections::HashMap<_, _> = url.query_pairs().collect();
    assert_eq!(fields["client_id"], "dynamic_agent_client");
    assert_eq!(fields["ext_agent_host_id"], "persistent-host");
    assert_eq!(fields["agent_name_hint"], "Slop Conductor");
    assert_eq!(
        fields["code_challenge"],
        URL_SAFE_NO_PAD.encode(Sha256::digest(login.verifier.as_bytes()))
    );
    let mut callback = reqwest::Url::parse(&login.redirect_uri).unwrap();
    callback.query_pairs_mut().extend_pairs([
        ("state", login.state.as_str()),
        ("code", "synthetic-code"),
        ("client_id", "oaiapp_fixture"),
    ]);
    assert_eq!(
        login.validate_callback(&callback).unwrap().1,
        "oaiapp_fixture"
    );
    let mut invalid = callback.clone();
    invalid
        .query_pairs_mut()
        .append_pair("state", "attacker-state");
    assert!(login.validate_callback(&invalid).is_err());
    let mut denied = reqwest::Url::parse(&login.redirect_uri).unwrap();
    denied
        .query_pairs_mut()
        .extend_pairs([("state", login.state.as_str()), ("error", "access_denied")]);
    assert!(login.validate_callback(&denied).is_err());
    let mut incomplete = reqwest::Url::parse(&login.redirect_uri).unwrap();
    incomplete
        .query_pairs_mut()
        .extend_pairs([("state", login.state.as_str()), ("code", "synthetic-code")]);
    assert!(login.validate_callback(&incomplete).is_err());
    let (received, reply) = tokio::join!(login.callback(), reqwest::get(callback));
    assert_eq!(received.unwrap().path(), "/auth/callback");
    assert!(reply.unwrap().status().is_success());
}

#[tokio::test]
async fn returning_login_reuses_registration_and_rejects_client_switch() {
    let connection = fixture_connection(PathBuf::new(), u64::MAX);
    let login = connection.begin_login().await.unwrap();
    let url = reqwest::Url::parse(login.authorization_url()).unwrap();
    let fields: std::collections::HashMap<_, _> = url.query_pairs().collect();
    assert_eq!(fields["client_id"], "oaiapp_fixture");
    assert!(!fields.contains_key("agent_name_hint"));
    let mut callback = reqwest::Url::parse(&login.redirect_uri).unwrap();
    callback
        .query_pairs_mut()
        .extend_pairs([("state", login.state.as_str()), ("code", "synthetic-code")]);
    assert_eq!(
        login.validate_callback(&callback).unwrap().1,
        "oaiapp_fixture"
    );
    callback
        .query_pairs_mut()
        .append_pair("client_id", "different-account");
    assert!(login.validate_callback(&callback).is_err());
}

#[test]
fn signed_id_tokens_require_correct_identity_claims_and_signature() {
    use jsonwebtoken::{EncodingKey, Header, encode};
    use rsa::{RsaPrivateKey, pkcs1::EncodeRsaPrivateKey, traits::PublicKeyParts};
    let private = RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048).unwrap();
    let jwks: JwkSet = serde_json::from_value(serde_json::json!({"keys":[{
        "kty":"RSA", "kid":"fixture", "alg":"RS256", "use":"sig",
        "n": URL_SAFE_NO_PAD.encode(private.n().to_bytes_be()),
        "e": URL_SAFE_NO_PAD.encode(private.e().to_bytes_be()),
    }]}))
    .unwrap();
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("fixture".to_owned());
    let key = EncodingKey::from_rsa_der(private.to_pkcs1_der().unwrap().as_bytes());
    let valid = serde_json::json!({"iss":ISSUER,"aud":"oaiapp_fixture","sub":"fixture-subject","nonce":"fixture-nonce","exp":now().unwrap()+3600});
    let token = encode(&header, &valid, &key).unwrap();
    assert_eq!(
        verify_identity(&token, "oaiapp_fixture", "fixture-nonce", &jwks).unwrap(),
        "fixture-subject"
    );
    for (field, value) in [
        ("iss", serde_json::json!("https://attacker.invalid")),
        ("aud", serde_json::json!("other-account")),
        ("nonce", serde_json::json!("other-nonce")),
        ("exp", serde_json::json!(0)),
    ] {
        let mut invalid = valid.clone();
        invalid[field] = value;
        let token = encode(&header, &invalid, &key).unwrap();
        assert!(verify_identity(&token, "oaiapp_fixture", "fixture-nonce", &jwks).is_err());
    }
    assert!(verify_identity("unsigned-token", "oaiapp_fixture", "fixture-nonce", &jwks).is_err());
}

#[tokio::test]
async fn concurrent_refresh_rotates_and_persists_once() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("account.json");
    let server = Server::new(vec![Reply::json(serde_json::json!({
        "access_token":"replacement-access", "refresh_token":"replacement-refresh",
        "token_type":"Bearer", "expires_in":3600, "scope":SCOPES,
    }))])
    .await;
    let mut connection = fixture_connection(path.clone(), 0);
    connection.token_endpoint = server.url.clone();
    let identity = connection.continuation_identity().await;
    let (first, second) = tokio::join!(connection.access_token(), connection.access_token());
    assert_eq!(first.unwrap(), "replacement-access");
    assert_eq!(second.unwrap(), "replacement-access");
    assert_eq!(connection.continuation_identity().await, identity);
    server.inspect(|requests| {
        assert_eq!(requests.len(), 1);
        let fields: std::collections::HashMap<_, _> =
            reqwest::Url::parse(&format!("http://fixture/?{}", requests[0].body))
                .unwrap()
                .query_pairs()
                .map(|(key, value)| (key.into_owned(), value.into_owned()))
                .collect();
        assert_eq!(fields["client_id"], "oaiapp_fixture");
        assert_eq!(fields["refresh_token"], "fixture-refresh-token");
        assert_eq!(fields["resource"], RESOURCE);
    });
    let restored = ChatGptConnection::load(&path).await.unwrap();
    assert_eq!(restored.continuation_identity().await, identity);
    assert_eq!(restored.access_token().await.unwrap(), "replacement-access");
    assert_eq!(
        restored.state.lock().await.credentials.refresh_token,
        "replacement-refresh"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn uncertain_or_rejected_refresh_is_not_replayed_or_exposed() {
    let server = Server::new(vec![Reply {
        status: axum::http::StatusCode::UNAUTHORIZED,
        content_type: "application/json",
        body: "fixture-refresh-token reflected secret".to_owned(),
        location: None,
    }])
    .await;
    let mut connection = fixture_connection(PathBuf::new(), 0);
    connection.token_endpoint = server.url.clone();
    for _ in 0..2 {
        let error = connection.access_token().await.unwrap_err();
        for rendered in [error.to_string(), format!("{error:?}")] {
            assert!(!rendered.contains("fixture-refresh-token"));
        }
    }
    server.inspect(|requests| assert_eq!(requests.len(), 1));
}
