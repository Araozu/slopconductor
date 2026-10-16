use super::*;
use hive_protocol::ErrorResponse;

const MAX_TOKEN_FILE_BYTES: u64 = 66;
const MAX_RESPONSE_BYTES: usize = 64 * 1024;

pub(super) fn read_token_file(path: &Path) -> Result<String, ClientError> {
    use std::io::Read;

    let link_metadata = std::fs::symlink_metadata(path)?;
    if !link_metadata.file_type().is_file() {
        return Err(ClientError::InvalidTokenFile);
    }
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(ClientError::InvalidTokenFile);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.mode() & 0o077 != 0 {
            return Err(ClientError::InvalidTokenFile);
        }
    }
    let mut bytes = Vec::with_capacity(66);
    file.take(MAX_TOKEN_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_TOKEN_FILE_BYTES {
        return Err(ClientError::InvalidTokenFile);
    }
    if bytes.ends_with(b"\r\n") {
        bytes.truncate(bytes.len() - 2);
    } else if bytes.ends_with(b"\n") {
        bytes.truncate(bytes.len() - 1);
    }
    if bytes.len() != 64 || !bytes.iter().all(u8::is_ascii_hexdigit) {
        return Err(ClientError::InvalidTokenFile);
    }
    String::from_utf8(bytes).map_err(|_| ClientError::InvalidTokenFile)
}

pub(super) async fn read_bounded(response: reqwest::Response) -> Result<Vec<u8>, ClientError> {
    read_bounded_to(response, MAX_RESPONSE_BYTES).await
}

pub(super) async fn read_bounded_to(
    mut response: reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>, ClientError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(ClientError::InvalidResponse);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

pub(super) fn api_error(status: u16, body: &[u8], token: Option<&HeaderValue>) -> ClientError {
    let parsed = serde_json::from_slice::<ErrorResponse>(body).ok();
    let (code, message) = parsed
        .map(|error| (error.code, error.message))
        .unwrap_or_else(|| ("http_error".to_owned(), "daemon request failed".to_owned()));
    let (code, message) = token
        .and_then(|value| value.to_str().ok())
        .map(|authorization| {
            let secret = authorization.trim_start_matches("Bearer ");
            (
                code.replace(authorization, "[REDACTED]")
                    .replace(secret, "[REDACTED]"),
                message
                    .replace(authorization, "[REDACTED]")
                    .replace(secret, "[REDACTED]"),
            )
        })
        .unwrap_or((code, message));
    if code == "peer_delivery_unknown" {
        return ClientError::DeliveryUncertain(message);
    }
    if status == 401 {
        ClientError::Unauthorized { code, message }
    } else {
        ClientError::Api {
            status,
            code,
            message,
        }
    }
}
