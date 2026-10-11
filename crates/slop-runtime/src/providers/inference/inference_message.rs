use super::{BlockContent, ContentBlock, Continuation, MessageRole};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InferenceMessage {
    pub role: MessageRole,
    pub blocks: Vec<ContentBlock>,
    pub continuation: Option<Continuation>,
}

impl InferenceMessage {
    pub fn text(role: MessageRole, id: String, text: String) -> Self {
        Self {
            role,
            blocks: vec![ContentBlock {
                id,
                content: BlockContent::Text { text },
            }],
            continuation: None,
        }
    }

    pub fn visible_text(&self) -> String {
        self.blocks
            .iter()
            .filter_map(|block| match &block.content {
                BlockContent::Text { text } | BlockContent::Refusal { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }
}
