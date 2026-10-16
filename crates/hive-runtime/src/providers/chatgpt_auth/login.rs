use super::callback::{Callback, callback_code, callback_fields};
use super::connection::ChatGptConnection;
use super::credentials::{Credentials, auth_error, random_value, save, token_exchange};
use super::identity::validate_identity;
use super::*;

/// A bounded, single-use login attempt with a loopback or manually submitted
/// callback. A client displays `authorization_url` for the user to open.
pub struct ChatGptLogin {
    callback: Callback,
    authorization_url: String,
    pub(super) redirect_uri: String,
    host_id: String,
    pub(super) state: String,
    nonce: String,
    pub(super) verifier: String,
    registration: Option<Credentials>,
}

impl ChatGptLogin {
    /// Start a new account registration. Persist/reuse a supported host URI
    /// (for example a UUIDv4 prefixed with `urn:uuid:`) for this daemon host;
    /// use `begin_login` for a returning registration.
    pub async fn begin(host_id: &str) -> Result<Self, ProviderError> {
        Self::start(host_id, None).await
    }

    /// Receive the final browser URL through a protected client channel without
    /// binding a callback socket. Uses the same OAuth grant and PKCE checks.
    pub async fn begin_headless(host_id: &str) -> Result<(Self, ChatGptCallback), ProviderError> {
        Self::start_headless(host_id, None).await
    }

    pub(super) async fn start(
        host_id: &str,
        registration: Option<Credentials>,
    ) -> Result<Self, ProviderError> {
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
        Self::prepare(
            host_id,
            registration,
            Callback::Loopback(listener),
            redirect_uri,
        )
    }

    pub(super) async fn start_headless(
        host_id: &str,
        registration: Option<Credentials>,
    ) -> Result<(Self, ChatGptCallback), ProviderError> {
        // Keep the supported loopback URI shape without binding a socket. A
        // random high port also avoids the usual local Codex callback listener.
        let mut bytes = [0_u8; 2];
        getrandom::fill(&mut bytes).map_err(|_| auth_error("random generation failed"))?;
        let port = 49152 + (u16::from_be_bytes(bytes) & 0x3fff);
        let (sender, receiver) = tokio::sync::mpsc::channel(1);
        let login = Self::prepare(
            host_id,
            registration,
            Callback::Manual(Mutex::new(receiver)),
            format!("http://127.0.0.1:{port}/auth/callback"),
        )?;
        let callback = ChatGptCallback {
            sender,
            redirect_uri: login.redirect_uri.clone(),
            state: login.state.clone(),
            client_id: login
                .registration
                .as_ref()
                .map(|record| record.client_id.clone()),
            submitted: std::sync::atomic::AtomicBool::new(false),
        };
        Ok((login, callback))
    }

    fn prepare(
        host_id: &str,
        registration: Option<Credentials>,
        callback: Callback,
        redirect_uri: String,
    ) -> Result<Self, ProviderError> {
        if host_id.is_empty() || host_id.len() > 256 {
            return Err(auth_error("host id is required and must be bounded"));
        }
        let state = random_value()?;
        let nonce = random_value()?;
        let verifier = random_value()?;
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut url = reqwest::Url::parse(AUTHORIZE_URL)
            .map_err(|_| auth_error("invalid authorization endpoint"))?;
        {
            let mut query = url.query_pairs_mut();
            if registration.is_none() {
                query.append_pair("agent_name_hint", "Hive");
            }
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
        }
        Ok(Self {
            callback,
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
        let fields = callback_fields(callback, &self.redirect_uri, &self.state)?;
        callback_code(
            &fields,
            self.registration
                .as_ref()
                .map(|record| record.client_id.as_str()),
        )
    }

    pub(super) async fn callback(&self) -> Result<reqwest::Url, ProviderError> {
        let listener = match &self.callback {
            Callback::Manual(receiver) => {
                return receiver
                    .lock()
                    .await
                    .recv()
                    .await
                    .ok_or_else(|| auth_error("headless callback channel closed"));
            }
            Callback::Loopback(listener) => listener,
        };
        loop {
            let (mut stream, _) = listener
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
