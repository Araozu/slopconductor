//! Structured, provider-neutral inference. One request is one model operation;
//! the daemon owns the surrounding tool loop and all side effects.

pub const MAX_BLOCKS: usize = 256;
pub const MAX_TOOL_CALLS: usize = 16;
pub const MAX_ARGUMENT_BYTES: usize = 64 * 1024;
/// Maximum image attachments accepted on one user message.
pub const MAX_IMAGES_PER_MESSAGE: usize = 16;
/// Conservative per-image context estimate in tokens. Image bytes are never
/// stored as base64 in SQLite or in the serialized inference view; budgeting
/// uses this fixed estimate instead of the wire base64 length.
pub const IMAGE_TOKEN_ESTIMATE: u64 = 2000;
/// Byte equivalent of [`IMAGE_TOKEN_ESTIMATE`] under the 1 token / 3 bytes heuristic.
pub const IMAGE_BYTE_ESTIMATE: u64 = IMAGE_TOKEN_ESTIMATE * 3;
/// Image media types accepted for upload and provider encoding.
pub const ALLOWED_IMAGE_MEDIA_TYPES: &[&str] =
    &["image/png", "image/jpeg", "image/gif", "image/webp"];
/// Maximum accepted upload size (20 MiB).
pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

mod generation_settings;
pub use generation_settings::GenerationSettings;
mod message_role;
pub use message_role::MessageRole;
mod content_block;
pub use content_block::ContentBlock;
mod block_content;
pub use block_content::BlockContent;
mod continuation;
pub use continuation::Continuation;
mod inference_message;
pub use inference_message::InferenceMessage;
mod tool_definition;
pub use tool_definition::ToolDefinition;
mod inference_request;
pub use inference_request::InferenceRequest;
pub use inference_request::ResolvedImage;
pub use inference_request::is_valid_artifact_id;
pub use inference_request::valid_name;
mod finish_reason;
pub use finish_reason::FinishReason;
mod inference_response;
pub use inference_response::InferenceResponse;
mod provider_event;
pub use provider_event::ProviderEvent;
mod model_capabilities;
pub use model_capabilities::ModelCapabilities;
mod capabilities;
pub use capabilities::capabilities;
