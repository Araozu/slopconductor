use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchCancelReceipt {
    pub command_id: String,
    pub batch_id: String,
    pub run_ids: Vec<String>,
}
