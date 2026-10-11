//! Bounded Git operations for daemon-managed workspaces.

mod error;
mod repository;
mod service;
#[cfg(test)]
mod tests;
mod workspace_diff;

pub use error::GitError;
pub use repository::Repository;
pub use service::GitService;
pub use workspace_diff::WorkspaceDiff;
