use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct ContentBlock {
    pub id: String,
    #[serde(flatten)]
    pub content: BlockContent,
}
