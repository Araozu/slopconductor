//! Bounded native HTTP transport shared by compiled-in provider clients.

use std::time::Duration;

use slop_core::provider::ProviderId;

use super::{ChatResponse, ProviderError, StreamDelta, WireProtocol, wire::*};

pub(super) fn http_client() -> Result<reqwest::Client, ProviderError> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("slopconductor/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()?)
}

pub(super) fn check_status(
    provider: ProviderId,
    response: &reqwest::Response,
) -> Result<(), ProviderError> {
    if response.status().is_success() {
        Ok(())
    } else {
        Err(ProviderError::UnexpectedStatus {
            provider,
            status: response.status().as_u16(),
        })
    }
}

pub(super) async fn read_body_limited(
    provider: ProviderId,
    mut response: reqwest::Response,
) -> Result<String, ProviderError> {
    check_status(provider, &response)?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len().saturating_add(chunk.len()) > MAX_BODY_BYTES {
            return Err(ProviderError::LimitExceeded {
                detail: "response body exceeds byte bound",
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| ProviderError::InvalidResponse {
        provider,
        detail: "response body is not valid UTF-8",
    })
}

pub(super) async fn read_streaming_response(
    provider: ProviderId,
    wire: WireProtocol,
    model: &str,
    mut response: reqwest::Response,
    on_delta: &mut (impl FnMut(StreamDelta) + Send),
) -> Result<ChatResponse, ProviderError> {
    check_status(provider, &response)?;
    if !is_event_stream(&response) {
        let body = read_body_limited(provider, response).await?;
        let value = serde_json::from_str(&body).map_err(|_| ProviderError::InvalidResponse {
            provider,
            detail: "response body is not JSON",
        })?;
        let result = decode_response(provider, wire, model, &value)?;
        if !result.text.is_empty() {
            on_delta(StreamDelta {
                text: result.text.clone(),
                reasoning: String::new(),
            });
        }
        return Ok(result);
    }
    let mut stream = SseStream::new(provider);
    let mut fold = StreamFold::new(provider);
    while let Some(chunk) = response.chunk().await? {
        for event in stream.push(&chunk)? {
            fold.feed(&event, wire, on_delta)?;
        }
    }
    for event in stream.finish()? {
        fold.feed(&event, wire, on_delta)?;
    }
    let outcome = fold.terminal.ok_or(ProviderError::InvalidResponse {
        provider,
        detail: "stream ended before terminal event",
    })?;
    Ok(ChatResponse {
        model: model.to_owned(),
        resolved_model: fold.resolved_model,
        text: fold.text,
        usage: fold.usage,
        wire,
        outcome,
    })
}
