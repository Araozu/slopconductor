use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ChildContext {
    #[serde(default)]
    pub message_ids: Vec<String>,
    #[serde(default)]
    pub artifact_ids: Vec<String>,
}
