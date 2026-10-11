use super::*;

/// Decoder failures have no service identity; the transport adds its descriptor.
#[derive(Debug)]
pub(in crate::providers) enum DecodeError {
    InvalidResponse { detail: &'static str },
    UnsupportedCapability { capability: &'static str },
    TurnFailed,
    LimitExceeded { detail: &'static str },
}

impl DecodeError {
    pub(in crate::providers) fn for_provider(self, provider: ProviderId) -> ProviderError {
        match self {
            Self::InvalidResponse { detail } => ProviderError::InvalidResponse { provider, detail },
            Self::UnsupportedCapability { capability } => {
                ProviderError::UnsupportedCapability { capability }
            }
            Self::TurnFailed => ProviderError::TurnFailed { provider },
            Self::LimitExceeded { detail } => ProviderError::LimitExceeded { detail },
        }
    }
}

pub(in crate::providers) fn is_event_stream(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|content_type| content_type.contains("text/event-stream"))
}

/// Bounded SSE event decoder that never splits UTF-8 across decodes.
///
/// CR/LF bytes cannot appear inside a multi-byte UTF-8 sequence. Decode only
/// complete lines, accepting LF, CRLF and CR even across network chunks.
pub(in crate::providers) struct SseStream {
    pending: Vec<u8>,
    event_data: Vec<String>,
    event_bytes: usize,
    total: usize,
}

impl SseStream {
    pub(in crate::providers) fn new() -> Self {
        Self {
            pending: Vec::new(),
            event_data: Vec::new(),
            event_bytes: 0,
            total: 0,
        }
    }

    /// Feed one network chunk; dispatch only blank-line-terminated events.
    pub(in crate::providers) fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, DecodeError> {
        self.total = self.total.saturating_add(chunk.len());
        if self.total > MAX_STREAM_BYTES {
            return Err(DecodeError::LimitExceeded {
                detail: "stream exceeds byte bound",
            });
        }
        self.pending.extend_from_slice(chunk);
        if self.pending.len() > MAX_STREAM_BYTES {
            return Err(DecodeError::LimitExceeded {
                detail: "stream exceeds byte bound",
            });
        }
        self.drain_lines(false)
    }

    /// Drain complete lines, rejecting overlong lines instead of buffering
    /// them without bound.
    fn drain_lines(&mut self, eof: bool) -> Result<Vec<String>, DecodeError> {
        let mut events = Vec::new();
        let mut consumed = 0;
        while let Some(offset) = self.pending[consumed..]
            .iter()
            .position(|b| *b == b'\n' || *b == b'\r')
        {
            let delimiter = consumed + offset;
            if self.pending[delimiter] == b'\r' && delimiter + 1 == self.pending.len() && !eof {
                break;
            }
            let delimiter_bytes = if self.pending[delimiter] == b'\r'
                && self.pending.get(delimiter + 1) == Some(&b'\n')
            {
                2
            } else {
                1
            };
            let end = delimiter + delimiter_bytes;
            if end - consumed > MAX_SSE_LINE_BYTES {
                return Err(DecodeError::LimitExceeded {
                    detail: "stream line exceeds byte bound",
                });
            }
            let line = std::str::from_utf8(&self.pending[consumed..end]).map_err(|_| {
                DecodeError::InvalidResponse {
                    detail: "stream is not valid UTF-8",
                }
            })?;
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if !self.event_data.is_empty() {
                    events.push(self.event_data.join("\n"));
                    self.event_data.clear();
                    self.event_bytes = 0;
                }
            } else if line.starts_with("data:") {
                self.event_bytes = self.event_bytes.saturating_add(line.len() + 1);
                if self.event_bytes > MAX_SSE_LINE_BYTES {
                    return Err(DecodeError::LimitExceeded {
                        detail: "stream event exceeds byte bound",
                    });
                }
                self.event_data.push(line.to_owned());
            }
            consumed = end;
        }
        self.pending.drain(..consumed);
        if self.pending.len() > MAX_SSE_LINE_BYTES {
            return Err(DecodeError::LimitExceeded {
                detail: "stream line exceeds byte bound",
            });
        }
        Ok(events)
    }

    /// EOF cannot dispatch an unterminated event as a completed response.
    pub(in crate::providers) fn finish(&mut self) -> Result<Vec<String>, DecodeError> {
        let events = self.drain_lines(true)?;
        if self.pending.is_empty() && self.event_data.is_empty() {
            return Ok(events);
        }
        Err(DecodeError::InvalidResponse {
            detail: "stream ended inside an SSE event",
        })
    }
}
