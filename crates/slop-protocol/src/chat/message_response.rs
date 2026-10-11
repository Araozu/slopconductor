use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MessageResponse {
    pub id: String,
    pub session_id: String,
    pub turn_id: String,
    pub role: String,
    pub text: String,
    pub status: String,
    #[serde(default)]
    pub blocks: Vec<crate::execution::ContentBlock>,
    #[serde(default)]
    pub request_id: Option<String>,
}
