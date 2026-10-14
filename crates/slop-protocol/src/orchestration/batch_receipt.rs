use super::TaskReceipt;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct BatchReceipt {
    pub command_id: String,
    pub batch_id: String,
    pub members: Vec<TaskReceipt>,
}
