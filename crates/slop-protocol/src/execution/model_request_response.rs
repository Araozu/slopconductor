use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelRequestResponse {
    pub id: String,
    pub turn_id: String,
    pub message_id: String,
    pub ordinal: u32,
    pub requested_model: String,
    pub resolved_model: Option<String>,
    pub requested_settings: GenerationSettings,
    pub effective_settings: GenerationSettings,
    pub status: String,
    pub finish_reason: Option<String>,
    pub usage: Option<crate::chat::UsageResponse>,
    pub error_code: Option<String>,
    #[serde(default)]
    pub compaction: bool,
}
