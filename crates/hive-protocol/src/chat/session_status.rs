use serde::{Deserialize, Serialize};

/// Daemon-derived execution/review status, independent of inbox organization flags.
/// Ready means successful completion newer than the shared inbox read cursor.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    #[default]
    Idle,
    Queued,
    Running,
    Paused,
    AwaitingChildren,
    Ready,
    Completed,
    Failed,
    Stopped,
}
