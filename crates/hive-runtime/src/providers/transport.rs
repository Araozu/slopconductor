//! Bounded native HTTP transport shared by compiled-in provider clients.

use std::time::Duration;

use hive_core::provider::ProviderId;

use super::{ChatResponse, ProviderError, StreamDelta, WireProtocol, wire::*};

pub(super) fn http_client() -> Result<reqwest::Client, ProviderError> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("hive/", env!("CARGO_PKG_VERSION")))
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()?)
}

pub(super) async fn check_status(
    provider: ProviderId,
    response: &mut reqwest::Response,
) -> Result<(), ProviderError> {
    if response.status().is_success() {
        Ok(())
    } else {
        let status = response.status().as_u16();
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs);
        // Read only a bounded error envelope, classify it, then discard it.
        // Provider messages can contain secrets and never enter diagnostics.
        let mut bytes = Vec::new();
        while bytes.len() < 8192 {
            let Some(chunk) = response.chunk().await? else {
                break;
            };
            bytes.extend_from_slice(&chunk[..chunk.len().min(8192 - bytes.len())]);
        }
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap_or_default();
        if is_context_error(&value) {
            return Err(ProviderError::ContextLengthExceeded);
        }
        let code = value
            .pointer("/error/code")
            .and_then(serde_json::Value::as_str);
        if matches!(
            code,
            Some("insufficient_quota" | "quota_exceeded" | "credit_balance_too_low")
        ) {
            return Err(ProviderError::QuotaExceeded);
        }
        if status == 429 {
            return Err(ProviderError::RateLimited { retry_after });
        }
        Err(ProviderError::UnexpectedStatus { provider, status })
    }
}

pub(super) fn is_context_error(value: &serde_json::Value) -> bool {
    let error = value
        .get("error")
        .or_else(|| value.pointer("/response/error"))
        .unwrap_or(value);
    let code = error.get("code").and_then(serde_json::Value::as_str);
    if matches!(
        code,
        Some("context_length_exceeded" | "context_window_exceeded" | "prompt_too_long")
    ) {
        return true;
    }
    error.get("type").and_then(serde_json::Value::as_str) == Some("invalid_request_error")
        && error
            .get("message")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|m| {
                let m = m.to_ascii_lowercase();
                m.contains("prompt is too long")
                    || m.contains("maximum context length")
                    || m.contains("exceeds the context window")
            })
}

pub(super) async fn read_body_limited(
    provider: ProviderId,
    mut response: reqwest::Response,
) -> Result<String, ProviderError> {
    check_status(provider, &mut response).await?;
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
    check_status(provider, &mut response).await?;
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
    let mut stream = SseStream::new();
    let mut fold = StreamFold::default();
    while let Some(chunk) = response.chunk().await? {
        for event in stream
            .push(&chunk)
            .map_err(|error| error.for_provider(provider))?
        {
            fold.feed(&event, wire, on_delta)
                .map_err(|error| error.for_provider(provider))?;
        }
    }
    for event in stream
        .finish()
        .map_err(|error| error.for_provider(provider))?
    {
        fold.feed(&event, wire, on_delta)
            .map_err(|error| error.for_provider(provider))?;
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
