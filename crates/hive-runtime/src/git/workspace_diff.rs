/// Current tracked and untracked changes in a managed worktree.
pub struct WorkspaceDiff {
    pub head_commit: String,
    pub patch: String,
    pub status: String,
}
