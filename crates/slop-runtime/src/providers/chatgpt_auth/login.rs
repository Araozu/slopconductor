use super::connection::ChatGptConnection;
use super::credentials::{Credentials, auth_error, random_value, save, token_exchange};
use super::identity::validate_identity;
use super::*;

/// A bounded, single-use loopback login attempt. No browser process is needed
/// on the daemon: a client displays `authorization_url` for the user to open.
pub struct ChatGptLogin {
    listener: TcpListener,
    authorization_url: String,
    pub(super) redirect_uri: String,
    host_id: String,
    pub(super) state: String,
    nonce: String,
    pub(super) verifier: String,
    registration: Option<Credentials>,
}

impl ChatGptLogin {
    /// Start a new account registration. Persist/reuse the same opaque host id
    /// for this daemon host; use `begin_login` for a returning registration.
    pub async fn begin(host_id: &str) -> Result<Self, ProviderError> {
        Self::start(host_id, None).await
    }

    pub(super) async fn start(
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
                query.append_pair("agent_name_hint", "Hive");
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

    pub(super) async fn authorize(&self) -> Result<Credentials, ProviderError> {
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
            revocation_confirmed: false,
        };
        token.apply(&mut credentials)?;
        Ok(credentials)
    }

    pub(super) fn validate_callback(
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

    pub(super) async fn callback(&self) -> Result<reqwest::Url, ProviderError> {
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
                "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Type: text/plain\r\nContent-Length: 53\r\n\r\nLogin callback received. Return to your Hive client.\n"
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
