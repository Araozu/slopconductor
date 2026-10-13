use serde::{Deserialize, Serialize};

/// Replace a user or assistant message in a new fork, discarding its suffix.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditMessageRequest {
    pub command_id: String,
    pub text: String,
    pub expected_revision: Option<u64>,
}
