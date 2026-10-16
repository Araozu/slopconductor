//! Workspaces are directories on the authoritative daemon. Worktrees are created explicitly.
mod open_workspace_request;
pub use open_workspace_request::OpenWorkspaceRequest;
mod create_workspace_request;
pub use create_workspace_request::CreateWorkspaceRequest;
mod create_worktree_request;
pub use create_worktree_request::CreateWorktreeRequest;
mod workspace_selection;
pub use workspace_selection::WorkspaceSelection;
mod workspace_response;
pub use workspace_response::WorkspaceResponse;
mod workspace_event_response;
pub use workspace_event_response::WorkspaceEventResponse;
mod workspace_diff_response;
pub use workspace_diff_response::WorkspaceDiffResponse;
mod remove_workspace_request;
pub use remove_workspace_request::RemoveWorkspaceRequest;
mod rename_workspace_request;
pub use rename_workspace_request::RenameWorkspaceRequest;
mod workspace_tool_limits;
pub use workspace_tool_limits::WorkspaceToolLimits;

/// Stable node-local workspace for threads without a directory.
pub const DEFAULT_WORKSPACE_ID: &str = "default";
