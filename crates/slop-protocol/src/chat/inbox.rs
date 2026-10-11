use serde::{Deserialize, Serialize};

/// Shared, daemon-owned conversation organization. Trash is restorable retention.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionInbox {
    pub starred: bool,
    pub archived: bool,
    pub trashed: bool,
    pub read_sequence: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UpdateSessionInboxRequest {
    pub command_id: String,
    pub starred: Option<bool>,
    pub archived: Option<bool>,
    pub trashed: Option<bool>,
    pub read_sequence: Option<u64>,
    /// Import old client state once without overwriting an existing daemon value.
    #[serde(default)]
    pub only_if_unset: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConversationDraft {
    pub id: String,
    pub title: String,
    pub text: String,
    pub model: String,
    pub workspace: String,
    pub settings: Option<crate::execution::GenerationSettings>,
    pub project: Option<crate::projects::ProjectWorkspaceRequest>,
    pub allowed_tools: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveConversationDraftRequest {
    pub draft: ConversationDraft,
    #[serde(default)]
    pub only_if_absent: bool,
}
