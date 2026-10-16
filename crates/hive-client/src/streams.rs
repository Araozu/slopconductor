use super::*;
use sha2::{Digest, Sha256};

pub(super) const MAX_EVENT_FRAME_BYTES: usize = 2 * 1024 * 1024;

/// A bounded NDJSON decoder for the daemon's reconnectable event endpoint.
pub struct EventStream {
    pub(super) response: reqwest::Response,
    pub(super) buffer: Vec<u8>,
    pub(super) finished: bool,
}

/// Bounded artifact download. Consume through EOF to validate the committed
/// checksum; callers should publish a destination only after validation.
pub struct ArtifactStream {
    pub metadata: hive_protocol::execution::ArtifactResponse,
    pub(super) response: reqwest::Response,
    pub(super) received: u64,
    pub(super) hash: Sha256,
    pub(super) finished: bool,
}
impl ArtifactStream {
    pub async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, ClientError> {
        if self.finished {
            return Ok(None);
        }
        let next = tokio::time::timeout(Duration::from_secs(45), self.response.chunk())
            .await
            .map_err(|_| ClientError::StreamTimeout)??;
        if let Some(chunk) = next {
            self.received = self.received.saturating_add(chunk.len() as u64);
            if self.received > self.metadata.size_bytes {
                return Err(ClientError::ArtifactIntegrity);
            }
            self.hash.update(&chunk);
            Ok(Some(chunk.to_vec()))
        } else {
            if self.received != self.metadata.size_bytes
                || format!("{:x}", self.hash.clone().finalize()) != self.metadata.sha256
            {
                return Err(ClientError::ArtifactIntegrity);
            }
            self.finished = true;
            Ok(None)
        }
    }
}

impl EventStream {
    pub async fn next_frame(&mut self) -> Result<Option<EventFrame>, ClientError> {
        loop {
            if let Some(newline) = self.buffer.iter().position(|byte| *byte == b'\n') {
                if newline > MAX_EVENT_FRAME_BYTES {
                    return Err(ClientError::InvalidEventFrame);
                }
                let mut line: Vec<u8> = self.buffer.drain(..=newline).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if line.is_empty() {
                    continue;
                }
                return serde_json::from_slice(&line)
                    .map(Some)
                    .map_err(|_| ClientError::InvalidEventFrame);
            }
            if self.buffer.len() > MAX_EVENT_FRAME_BYTES {
                return Err(ClientError::InvalidEventFrame);
            }
            if self.finished {
                return Ok(None);
            }
            match tokio::time::timeout(Duration::from_secs(45), self.response.chunk())
                .await
                .map_err(|_| ClientError::StreamTimeout)??
            {
                Some(chunk) => {
                    self.buffer.extend_from_slice(&chunk);
                }
                None => {
                    self.finished = true;
                    if self.buffer.is_empty() {
                        return Ok(None);
                    }
                    let line = std::mem::take(&mut self.buffer);
                    if line.len() > MAX_EVENT_FRAME_BYTES {
                        return Err(ClientError::InvalidEventFrame);
                    }
                    return serde_json::from_slice(&line)
                        .map(Some)
                        .map_err(|_| ClientError::InvalidEventFrame);
                }
            }
        }
    }
}
