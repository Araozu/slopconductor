use super::credentials::auth_error;
use super::*;

pub(super) enum Callback {
    Loopback(TcpListener),
    Manual(Mutex<tokio::sync::mpsc::Receiver<reqwest::Url>>),
}

/// Only the daemon's authenticated API may submit this single-use callback.
/// No Debug implementation: the submitted URL contains an authorization code.
pub struct ChatGptCallback {
    pub(super) sender: tokio::sync::mpsc::Sender<reqwest::Url>,
    pub(super) redirect_uri: String,
    pub(super) state: String,
    pub(super) client_id: Option<String>,
    pub(super) submitted: std::sync::atomic::AtomicBool,
}

impl ChatGptCallback {
    pub fn submit(&self, value: &str) -> Result<(), ProviderError> {
        if value.len() > 16 * 1024 {
            return Err(auth_error("callback URL exceeds byte bound"));
        }
        let url = reqwest::Url::parse(value).map_err(|_| auth_error("invalid callback URL"))?;
        let fields = callback_fields(&url, &self.redirect_uri, &self.state)?;
        if !fields.contains_key("error") {
            callback_code(&fields, self.client_id.as_deref())?;
        }
        if self
            .submitted
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return Err(auth_error("callback already submitted"));
        }
        self.sender
            .try_send(url)
            .map_err(|_| auth_error("callback already submitted or login ended"))
    }
}

pub(super) fn callback_fields(
    callback: &reqwest::Url,
    redirect_uri: &str,
    state: &str,
) -> Result<std::collections::HashMap<String, String>, ProviderError> {
    let expected =
        reqwest::Url::parse(redirect_uri).map_err(|_| auth_error("invalid callback URI"))?;
    if callback.origin() != expected.origin()
        || callback.path() != expected.path()
        || !callback.username().is_empty()
        || callback.password().is_some()
        || callback.fragment().is_some()
    {
        return Err(auth_error("callback URI does not match"));
    }
    let mut fields = std::collections::HashMap::new();
    for (key, value) in callback.query_pairs() {
        if fields
            .insert(key.into_owned(), value.into_owned())
            .is_some()
        {
            return Err(auth_error("duplicate callback parameter"));
        }
    }
    if fields.get("state").map(String::as_str) != Some(state) {
        return Err(auth_error("callback state does not match"));
    }
    Ok(fields)
}

pub(super) fn callback_code(
    fields: &std::collections::HashMap<String, String>,
    registered_client_id: Option<&str>,
) -> Result<(String, String), ProviderError> {
    if fields.contains_key("error") {
        return Err(auth_error("authorization was denied"));
    }
    let client_id = match registered_client_id {
        Some(id) => {
            if fields.get("client_id").is_some_and(|value| value != id) {
                return Err(auth_error("callback client id does not match"));
            }
            id.to_owned()
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
