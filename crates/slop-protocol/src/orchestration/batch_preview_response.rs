use super::{BatchSpec, CombinationResponse};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct BatchPreviewResponse {
    /// Effective settings and exact base commit are safe to submit unchanged.
    pub spec: BatchSpec,
    pub combinations: Vec<CombinationResponse>,
}
