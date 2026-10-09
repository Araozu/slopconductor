//! Native Sign in with ChatGPT for a daemon-owned, renewable connection.
//!
//! https://developers.openai.com/siwc/token-sharing-open-source/sign-in
//! https://developers.openai.com/siwc/token-sharing-open-source/profiles-and-sessions

use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header, jwk::JwkSet};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Mutex,
};

use super::{ProviderError, transport};

const ISSUER: &str = "https://auth.openai.com";
const AUTHORIZE_URL: &str = "https://auth.openai.com/api/accounts/authorize";
const TOKEN_URL: &str = "https://auth.openai.com/api/accounts/oauth/token";
const RESOURCE: &str = "https://api.openai.com/v1";
const SCOPES: &str =
    "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const MAX_CREDENTIAL_BYTES: u64 = 64 * 1024;

fn auth_error(detail: &'static str) -> ProviderError {
    ProviderError::Authentication { detail }
}

fn now() -> Result<u64, ProviderError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| auth_error("system clock is invalid"))
}

fn random_value() -> Result<String, ProviderError> {
    let mut bytes = [0; 32];
    getrandom::fill(&mut bytes).map_err(|_| auth_error("random generation failed"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// Credentials are private runtime data, separate from conversation context.
/// No Debug implementation: tokens must never enter diagnostics.
#[derive(Clone, Serialize, Deserialize)]
struct Credentials {
    issuer: String,
    subject: String,
    client_id: String,
    ext_agent_host_id: String,
    id_token: String,
    access_token: String,
    refresh_token: String,
    scopes: Vec<String>,
    expires_at: u64,
}

impl Credentials {
    fn validate(&self) -> Result<(), ProviderError> {
        if self.issuer != ISSUER
            || self.subject.is_empty()
            || self.client_id.is_empty()
            || self.client_id == "dynamic_agent_client"
            || self.ext_agent_host_id.is_empty()
            || self.id_token.is_empty()
            || self.access_token.is_empty()
            || self.refresh_token.is_empty()
            || !self
                .scopes
                .iter()
                .any(|scope| scope == "chatgpt.tokens.use.direct")
            || !self.scopes.iter().any(|scope| scope == "offline_access")
            || !self.scopes.iter().any(|scope| scope == "resource.invoke")
        {
            return Err(auth_error(
                "credential record is incomplete or plan usage is not authorized",
            ));
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    id_token: Option<String>,
    token_type: String,
    expires_in: u64,
    scope: String,
}

impl TokenResponse {
    fn apply(self, credentials: &mut Credentials) -> Result<(), ProviderError> {
        if !self.token_type.eq_ignore_ascii_case("Bearer") || self.expires_in == 0 {
            return Err(auth_error("invalid token response"));
        }
        credentials.access_token = self.access_token;
        credentials.refresh_token = self.refresh_token;
        if let Some(id_token) = self.id_token {
            credentials.id_token = id_token;
        }
        credentials.expires_at = now()?
            .checked_add(self.expires_in)
            .ok_or_else(|| auth_error("invalid token expiration"))?;
        credentials.scopes = self.scope.split_whitespace().map(str::to_owned).collect();
        credentials.validate()
    }
}

async fn token_exchange(
    http: &reqwest::Client,
    endpoint: &str,
    fields: &[(&str, &str)],
) -> Result<TokenResponse, ProviderError> {
    let response = http.post(endpoint).form(fields).send().await?;
    if !response.status().is_success() {
        return Err(auth_error("token exchange rejected; sign in again"));
    }
    let body = transport::read_body_limited(super::ProviderId::Codex, response).await?;
    if body.len() as u64 > MAX_CREDENTIAL_BYTES {
        return Err(auth_error("token response exceeds byte bound"));
    }
    serde_json::from_str(&body).map_err(|_| auth_error("invalid token response"))
}

async fn save(path: PathBuf, credentials: Credentials) -> Result<(), ProviderError> {
    tokio::task::spawn_blocking(move || {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .ok_or(ProviderError::CredentialStorage)?;
        // The caller supplies an existing private directory. NamedTempFile
        // atomically replaces the record, including on Windows.
        let mut file = tempfile::NamedTempFile::new_in(parent)
            .map_err(|_| ProviderError::CredentialStorage)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|_| ProviderError::CredentialStorage)?;
        }
        let bytes =
            serde_json::to_vec(&credentials).map_err(|_| ProviderError::CredentialStorage)?;
        if bytes.len() as u64 > MAX_CREDENTIAL_BYTES {
            return Err(ProviderError::CredentialStorage);
        }
        file.write_all(&bytes)
            .map_err(|_| ProviderError::CredentialStorage)?;
        file.as_file()
            .sync_all()
            .map_err(|_| ProviderError::CredentialStorage)?;
        file.persist(&path)
            .map_err(|_| ProviderError::CredentialStorage)?;
        #[cfg(unix)]
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| ProviderError::CredentialStorage)?;
        Ok(())
    })
    .await
    .map_err(|_| ProviderError::CredentialStorage)?
}

struct TokenState {
    credentials: Credentials,
    refresh_blocked: bool,
}

/// One account registration. Share this object across clients/sessions so token
/// rotation is serialized. The daemon must exclusively own its credential file.
pub struct ChatGptConnection {
    http: reqwest::Client,
    token_endpoint: String,
    path: PathBuf,
    state: Mutex<TokenState>,
}

impl ChatGptConnection {
    /// Restore a credential record created by a validated login in this app.
    pub async fn load(path: impl AsRef<Path>) -> Result<Self, ProviderError> {
        let path = path.as_ref().to_owned();
        let read_path = path.clone();
        let credentials: Credentials = tokio::task::spawn_blocking(move || {
            let file =
                std::fs::File::open(read_path).map_err(|_| ProviderError::CredentialStorage)?;
            let mut body = Vec::new();
            file.take(MAX_CREDENTIAL_BYTES + 1)
                .read_to_end(&mut body)
                .map_err(|_| ProviderError::CredentialStorage)?;
            if body.len() as u64 > MAX_CREDENTIAL_BYTES {
                return Err(ProviderError::CredentialStorage);
            }
            serde_json::from_slice(&body).map_err(|_| ProviderError::CredentialStorage)
        })
        .await
        .map_err(|_| ProviderError::CredentialStorage)??;
        credentials.validate()?;
        Ok(Self::from_credentials(
            path,
            credentials,
            transport::http_client()?,
        ))
    }

    fn from_credentials(path: PathBuf, credentials: Credentials, http: reqwest::Client) -> Self {
        Self {
            http,
            path,
            token_endpoint: TOKEN_URL.to_owned(),
            state: Mutex::new(TokenState {
                credentials,
                refresh_blocked: false,
            }),
        }
    }

    /// Reauthorize the selected registration with its saved client and host ids.
    pub async fn begin_login(&self) -> Result<ChatGptLogin, ProviderError> {
        let state = self.state.lock().await;
        ChatGptLogin::start(
            &state.credentials.ext_agent_host_id,
            Some(state.credentials.clone()),
        )
        .await
    }

    pub(super) async fn access_token(&self) -> Result<String, ProviderError> {
        let mut state = self.state.lock().await;
        if state.credentials.expires_at > now()?.saturating_add(60) {
            return Ok(state.credentials.access_token.clone());
        }
        if state.refresh_blocked {
            return Err(auth_error("token renewal requires a new sign-in"));
        }
        // If dispatch, parsing, or persistence fails, a rotating refresh token
        // may have been consumed. Never automatically replay that operation.
        state.refresh_blocked = true;
        let credentials = &state.credentials;
        let token = token_exchange(
            &self.http,
            &self.token_endpoint,
            &[
                ("grant_type", "refresh_token"),
                ("client_id", &credentials.client_id),
                ("refresh_token", &credentials.refresh_token),
                ("resource", RESOURCE),
            ],
        )
        .await?;
        let mut replacement = state.credentials.clone();
        token.apply(&mut replacement)?;
        save(self.path.clone(), replacement.clone()).await?;
        state.credentials = replacement;
        state.refresh_blocked = false;
        Ok(state.credentials.access_token.clone())
    }
}

/// A bounded, single-use loopback login attempt. No browser process is needed
/// on the daemon: a client displays `authorization_url` for the user to open.
pub struct ChatGptLogin {
    listener: TcpListener,
    authorization_url: String,
    redirect_uri: String,
    host_id: String,
    state: String,
    nonce: String,
    verifier: String,
    registration: Option<Credentials>,
}

impl ChatGptLogin {
    /// Start a new account registration. Persist/reuse the same opaque host id
    /// for this daemon host; use `begin_login` for a returning registration.
    pub async fn begin(host_id: &str) -> Result<Self, ProviderError> {
        Self::start(host_id, None).await
    }

    async fn start(
        host_id: &str,
        registration: Option<Credentials>,
    ) -> Result<Self, ProviderError> {
        if host_id.is_empty() || host_id.len() > 256 {
            return Err(auth_error("host id is required and must be bounded"));
        }
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| auth_error("loopback callback listener failed"))?;
        let redirect_uri = format!(
            "http://127.0.0.1:{}/auth/callback",
            listener
                .local_addr()
                .map_err(|_| auth_error("callback address unavailable"))?
                .port()
        );
        let state = random_value()?;
        let nonce = random_value()?;
        let verifier = random_value()?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut url = reqwest::Url::parse(AUTHORIZE_URL)
            .map_err(|_| auth_error("invalid authorization endpoint"))?;
        {
            let mut query = url.query_pairs_mut();
            query.extend_pairs([
                (
                    "client_id",
                    registration
                        .as_ref()
                        .map_or("dynamic_agent_client", |record| record.client_id.as_str()),
                ),
                ("ext_agent_host_id", host_id),
                ("response_type", "code"),
                ("redirect_uri", &redirect_uri),
                ("scope", SCOPES),
                ("resource", RESOURCE),
                ("state", &state),
                ("nonce", &nonce),
                ("code_challenge_method", "S256"),
                ("code_challenge", &challenge),
            ]);
            if registration.is_none() {
                query.append_pair("agent_name_hint", "Slop Conductor");
            }
        }
        Ok(Self {
            listener,
            authorization_url: url.into(),
            redirect_uri,
            host_id: host_id.to_owned(),
            state,
            nonce,
            verifier,
            registration,
        })
    }

    #[must_use]
    pub fn authorization_url(&self) -> &str {
        &self.authorization_url
    }

    /// Wait up to five minutes, validate the callback and signed ID token, then
    /// atomically store credentials. The parent directory must already exist.
    pub async fn finish(self, path: impl AsRef<Path>) -> Result<ChatGptConnection, ProviderError> {
        self.finish_with_cancellation(path, std::future::pending())
            .await
    }

    /// Cancel authorization on daemon shutdown, but drain any credential write
    /// already begun. An interrupted exchange is never automatically replayed.
    pub async fn finish_with_cancellation(
        self,
        path: impl AsRef<Path>,
        cancellation: impl std::future::Future<Output = ()>,
    ) -> Result<ChatGptConnection, ProviderError> {
        let credentials = tokio::select! {
            biased;
            _ = cancellation => return Err(auth_error("login interrupted")),
            credentials = self.authorize() => credentials?,
        };
        let path = path.as_ref().to_owned();
        save(path.clone(), credentials.clone()).await?;
        Ok(ChatGptConnection::from_credentials(
            path,
            credentials,
            transport::http_client()?,
        ))
    }

    async fn authorize(&self) -> Result<Credentials, ProviderError> {
        let callback = tokio::time::timeout(Duration::from_secs(300), self.callback())
            .await
            .map_err(|_| auth_error("login timed out"))??;
        let (code, client_id) = self.validate_callback(&callback)?;
        let http = transport::http_client()?;
        let token = token_exchange(
            &http,
            TOKEN_URL,
            &[
                ("grant_type", "authorization_code"),
                ("client_id", &client_id),
                ("code", &code),
                ("code_verifier", &self.verifier),
                ("redirect_uri", &self.redirect_uri),
                ("resource", RESOURCE),
            ],
        )
        .await?;
        let id_token = token
            .id_token
            .as_deref()
            .ok_or_else(|| auth_error("ID token is missing"))?;
        let subject = validate_identity(&http, id_token, &client_id, &self.nonce).await?;
        if self
            .registration
            .as_ref()
            .is_some_and(|record| record.subject != subject)
        {
            return Err(auth_error("returning account identity does not match"));
        }
        let mut credentials = Credentials {
            issuer: ISSUER.to_owned(),
            subject,
            client_id,
            ext_agent_host_id: self.host_id.clone(),
            id_token: String::new(),
            access_token: String::new(),
            refresh_token: String::new(),
            scopes: Vec::new(),
            expires_at: 0,
        };
        token.apply(&mut credentials)?;
        Ok(credentials)
    }

    fn validate_callback(
        &self,
        callback: &reqwest::Url,
    ) -> Result<(String, String), ProviderError> {
        let mut fields = std::collections::HashMap::new();
        for (key, value) in callback.query_pairs() {
            if fields
                .insert(key.into_owned(), value.into_owned())
                .is_some()
            {
                return Err(auth_error("duplicate callback parameter"));
            }
        }
        if fields.get("state") != Some(&self.state) {
            return Err(auth_error("callback state does not match"));
        }
        if fields.contains_key("error") {
            return Err(auth_error("authorization was denied"));
        }
        let client_id = match &self.registration {
            Some(record) => {
                if fields
                    .get("client_id")
                    .is_some_and(|id| id != &record.client_id)
                {
                    return Err(auth_error("callback client id does not match"));
                }
                record.client_id.clone()
            }
            None => fields
                .get("client_id")
                .filter(|id| !id.is_empty() && *id != "dynamic_agent_client")
                .cloned()
                .ok_or_else(|| auth_error("registration did not issue a client id"))?,
        };
        let code = fields
            .get("code")
            .filter(|code| !code.is_empty())
            .cloned()
            .ok_or_else(|| auth_error("authorization code is missing"))?;
        Ok((code, client_id))
    }

    async fn callback(&self) -> Result<reqwest::Url, ProviderError> {
        loop {
            let (mut stream, _) = self
                .listener
                .accept()
                .await
                .map_err(|_| auth_error("callback accept failed"))?;
            let mut request = Vec::new();
            tokio::time::timeout(Duration::from_secs(10), async {
                let mut chunk = [0; 1024];
                while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let count = stream
                        .read(&mut chunk)
                        .await
                        .map_err(|_| auth_error("callback read failed"))?;
                    if count == 0 || request.len() + count > 16 * 1024 {
                        return Err(auth_error("invalid callback HTTP request"));
                    }
                    request.extend_from_slice(&chunk[..count]);
                }
                Ok(())
            })
            .await
            .map_err(|_| auth_error("callback read timed out"))??;
            let request = std::str::from_utf8(&request)
                .map_err(|_| auth_error("invalid callback encoding"))?;
            let mut parts = request
                .lines()
                .next()
                .unwrap_or_default()
                .split_whitespace();
            let method = parts.next();
            let target = parts.next().unwrap_or_default();
            let url = reqwest::Url::parse(&format!(
                "{}{}",
                self.redirect_uri.trim_end_matches("/auth/callback"),
                target
            ))
            .map_err(|_| auth_error("invalid callback URL"))?;
            let valid = method == Some("GET")
                && target.starts_with("/auth/callback?")
                && url.path() == "/auth/callback";
            let reply = if valid {
                "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: text/plain\r\nContent-Length: 53\r\n\r\nLogin callback received. Return to your Slop client.\n"
            } else {
                "HTTP/1.1 404 Not Found\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
            };
            stream
                .write_all(reply.as_bytes())
                .await
                .map_err(|_| auth_error("callback reply failed"))?;
            if valid {
                return Ok(url);
            }
        }
    }
}

async fn validate_identity(
    http: &reqwest::Client,
    token: &str,
    client_id: &str,
    nonce: &str,
) -> Result<String, ProviderError> {
    #[derive(Deserialize)]
    struct Discovery {
        jwks_uri: String,
    }
    let response = http
        .get(format!("{ISSUER}/.well-known/openid-configuration"))
        .send()
        .await?;
    let body = transport::read_body_limited(super::ProviderId::Codex, response).await?;
    let discovery: Discovery =
        serde_json::from_str(&body).map_err(|_| auth_error("invalid OIDC discovery"))?;
    let url =
        reqwest::Url::parse(&discovery.jwks_uri).map_err(|_| auth_error("invalid JWKS URL"))?;
    if url.scheme() != "https"
        || url.host_str() != Some("auth.openai.com")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(auth_error("untrusted JWKS endpoint"));
    }
    let response = http.get(url).send().await?;
    let body = transport::read_body_limited(super::ProviderId::Codex, response).await?;
    let jwks: JwkSet =
        serde_json::from_str(&body).map_err(|_| auth_error("invalid JWKS response"))?;
    verify_identity(token, client_id, nonce, &jwks)
}

fn verify_identity(
    token: &str,
    client_id: &str,
    nonce: &str,
    jwks: &JwkSet,
) -> Result<String, ProviderError> {
    #[derive(Deserialize)]
    struct Claims {
        sub: String,
        nonce: String,
    }
    let header = decode_header(token).map_err(|_| auth_error("invalid ID token"))?;
    if header.alg != Algorithm::RS256 {
        return Err(auth_error("unsupported ID token signature"));
    }
    let key = header
        .kid
        .as_deref()
        .and_then(|kid| jwks.find(kid))
        .ok_or_else(|| auth_error("ID token signing key is missing"))?;
    let key = DecodingKey::from_jwk(key).map_err(|_| auth_error("invalid signing key"))?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_issuer(&[ISSUER]);
    validation.set_audience(&[client_id]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    let claims = decode::<Claims>(token, &key, &validation)
        .map_err(|_| auth_error("ID token validation failed"))?
        .claims;
    if claims.nonce != nonce || claims.sub.is_empty() {
        return Err(auth_error("ID token identity or nonce does not match"));
    }
    Ok(claims.sub)
}

#[cfg(test)]
pub(super) fn fixture_connection(path: PathBuf, expires_at: u64) -> ChatGptConnection {
    let credentials = Credentials {
        issuer: ISSUER.to_owned(),
        subject: "fixture-subject".to_owned(),
        client_id: "oaiapp_fixture".to_owned(),
        ext_agent_host_id: "fixture-host".to_owned(),
        id_token: "fixture-id-token".to_owned(),
        access_token: "fixture-access-token".to_owned(),
        refresh_token: "fixture-refresh-token".to_owned(),
        scopes: SCOPES.split_whitespace().map(str::to_owned).collect(),
        expires_at,
    };
    ChatGptConnection::from_credentials(path, credentials, transport::http_client().unwrap())
}

#[cfg(test)]
mod tests {
    use super::super::test_http::{Reply, Server};
    use super::*;

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
        assert!(
            verify_identity("unsigned-token", "oaiapp_fixture", "fixture-nonce", &jwks).is_err()
        );
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
        let (first, second) = tokio::join!(connection.access_token(), connection.access_token());
        assert_eq!(first.unwrap(), "replacement-access");
        assert_eq!(second.unwrap(), "replacement-access");
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
}
