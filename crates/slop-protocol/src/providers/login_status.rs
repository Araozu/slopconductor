use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoginStatus {
    pub login_id: String,
    pub status: String,
    pub error_code: Option<String>,
}
