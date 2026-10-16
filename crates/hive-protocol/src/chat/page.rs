use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub next_after: Option<u64>,
}
