use std::{future::Future, pin::Pin};

use super::{
    AuthMode, ChatMessage, ChatRequest, ChatResponse, Provider, ProviderError, Role, StreamDelta,
    TurnOutcome, WireProtocol, inference,
};

pub type ProviderFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, ProviderError>> + Send + 'a>>;

/// Shared native execution interface for structured inference and text clients.
///
/// The metadata registry and authenticated clients have separate lifetimes.
/// This object-safe interface lets execution select a client without matching
/// its concrete integration. Structured adapters preserve private continuation;
/// dropping an inference future aborts local transport. Account discovery beyond
/// model listing remains a separate credential-management operation.
pub trait ProviderClient: Send + Sync {
    fn descriptor(&self) -> &dyn Provider;

    fn capabilities(&self, _model: &str) -> inference::ModelCapabilities {
        inference::ModelCapabilities {
            tools: false,
            reasoning_efforts: Vec::new(),
        }
    }

    /// One inference attempt. Dropping this future cancels local transport;
    /// adapters never execute tools or retry an inference behind the supervisor.
    fn infer<'a>(
        &'a self,
        request: &'a inference::InferenceRequest,
        on_event: &'a mut (dyn FnMut(inference::ProviderEvent) + Send),
    ) -> ProviderFuture<'a, inference::InferenceResponse> {
        Box::pin(async move {
            use inference::*;
            request.validate(self.descriptor(), &self.capabilities(&request.model))?;
            let mut messages = Vec::new();
            for message in &request.messages {
                if message
                    .blocks
                    .iter()
                    .any(|block| !matches!(block.content, BlockContent::Text { .. }))
                    || message
                        .continuation
                        .as_ref()
                        .is_some_and(|continuation| continuation.required)
                {
                    return Err(ProviderError::UnsupportedCapability {
                        capability: "structured history",
                    });
                }
                let role = match message.role {
                    inference::MessageRole::System => Role::System,
                    inference::MessageRole::Developer => Role::Developer,
                    inference::MessageRole::User => Role::User,
                    inference::MessageRole::Assistant => Role::Assistant,
                    inference::MessageRole::Tool => {
                        return Err(ProviderError::UnsupportedCapability {
                            capability: "tool results",
                        });
                    }
                };
                messages.push(ChatMessage {
                    role,
                    content: message.visible_text(),
                });
            }
            let legacy = ChatRequest {
                model: request.model.clone(),
                messages,
                max_tokens: request.settings.max_output_tokens,
                session_id: request.session_id.clone(),
            };
            let result = self
                .complete_streaming(&legacy, &mut |delta| {
                    if !delta.text.is_empty() {
                        on_event(inference::ProviderEvent::TextDelta {
                            block_index: 0,
                            text: delta.text,
                        });
                    }
                })
                .await?;
            Ok(InferenceResponse {
                resolved_model: result.resolved_model,
                message: inference::InferenceMessage::text(
                    inference::MessageRole::Assistant,
                    format!("{}:block:0", request.request_id),
                    result.text,
                ),
                usage: result.usage,
                finish_reason: match result.outcome {
                    TurnOutcome::Completed => inference::FinishReason::Stop,
                    TurnOutcome::Incomplete { .. } => inference::FinishReason::OutputLimit,
                },
            })
        })
    }

    fn auth_mode(&self) -> AuthMode {
        AuthMode::ApiKey
    }

    /// Validate before any inference network access. No automatic fallback.
    fn validate(&self, request: &ChatRequest) -> Result<WireProtocol, ProviderError> {
        request.validate()?;
        if self.descriptor().requires_output_limit() && request.max_tokens.is_none() {
            return Err(ProviderError::InvalidRequest("max_tokens is required"));
        }
        let wire = self.descriptor().wire_protocol(&request.model)?;
        if wire == WireProtocol::AnthropicMessages {
            if request
                .messages
                .iter()
                .any(|message| message.role == Role::Developer)
            {
                return Err(ProviderError::UnsupportedCapability {
                    capability: "developer messages for Anthropic Messages",
                });
            }
            // The current adapter accepts one optional leading system message.
            // Hoisting later instructions or joining messages would change the
            // supplied context without an explicit conversion policy.
            if request
                .messages
                .iter()
                .skip(1)
                .any(|message| message.role == Role::System)
            {
                return Err(ProviderError::UnsupportedCapability {
                    capability: "multiple or non-leading system messages",
                });
            }
            if request.messages.iter().all(|m| m.role == Role::System) {
                return Err(ProviderError::InvalidRequest(
                    "messages require a user or assistant turn",
                ));
            }
        }
        Ok(wire)
    }

    fn list_models(&self) -> ProviderFuture<'_, Vec<String>>;
    fn complete<'a>(&'a self, request: &'a ChatRequest) -> ProviderFuture<'a, ChatResponse>;
    fn complete_streaming<'a>(
        &'a self,
        request: &'a ChatRequest,
        on_delta: &'a mut (dyn FnMut(StreamDelta) + Send),
    ) -> ProviderFuture<'a, ChatResponse>;
}
