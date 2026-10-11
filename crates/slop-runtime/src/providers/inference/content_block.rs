use super::BlockContent;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContentBlock {
    pub id: String,
    #[serde(flatten)]
    pub content: BlockContent,
}
