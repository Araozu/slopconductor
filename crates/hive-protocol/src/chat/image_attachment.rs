use serde::{Deserialize, Serialize};

/// Reference to an uploaded image artifact attached to a user message.
/// The bytes are fetched via `GET /v1/artifacts/{id}/content`; only the
/// content-addressed id travels in the message payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct ImageAttachment {
    pub artifact_id: String,
}
