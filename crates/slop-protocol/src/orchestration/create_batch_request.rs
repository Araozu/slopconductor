use super::BatchSpec;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateBatchRequest {
    pub command_id: String,
    pub spec: BatchSpec,
}
