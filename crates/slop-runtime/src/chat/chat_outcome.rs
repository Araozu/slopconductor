use crate::providers::Usage;

use super::ChatStatus;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatOutcome {
    pub text: String,
    pub resolved_model: Option<String>,
    pub usage: Usage,
    pub status: ChatStatus,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
}
