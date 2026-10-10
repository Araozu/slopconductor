//! Bounded Git subprocesses for daemon-managed workspaces. No agent CLI is used.
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use process_wrap::tokio::*;
use tokio::{
    io::AsyncReadExt,
    sync::{Mutex, Semaphore, watch},
};

const OUTPUT_LIMIT: usize = 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, thiserror::Error)]
#[error("{code}")]
pub struct GitError {
    pub code: &'static str,
    pub effects_unknown: bool,
}
impl GitError {
    fn known(code: &'static str) -> Self {
        Self {
            code,
            effects_unknown: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Repository {
    pub path: String,
    pub common_dir: String,
}

pub struct WorkspaceDiff {
    pub head_commit: String,
    pub patch: String,
    pub status: String,
}

pub struct GitService {
    data_dir: PathBuf,
    slots: Arc<Semaphore>,
    mutations: Mutex<()>,
}

struct ChildGuard(Box<dyn ChildWrapper>);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

impl GitService {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            slots: Arc::new(Semaphore::new(2)),
            mutations: Mutex::new(()),
        }
    }

    async fn run(
        &self,
        cwd: &Path,
        args: &[&str],
        mutation: bool,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<Vec<u8>, GitError> {
        let _slot = tokio::select! {
            slot = self.slots.acquire() => slot.map_err(|_| GitError::known("git_unavailable"))?,
            _ = cancel.changed() => return Err(GitError::known("cancelled")),
        };
        if *cancel.borrow() {
            return Err(GitError::known("cancelled"));
        }
        // An empty hooks directory prevents post-checkout hooks from acquiring
        // independent lifetime. Provider credentials never enter this environment.
        let hooks = self.data_dir.join("git-hooks");
        tokio::fs::create_dir_all(&hooks)
            .await
            .map_err(|_| GitError::known("workspace_path_denied"))?;
        if tokio::fs::canonicalize(&hooks).await.ok().as_ref() != Some(&hooks) {
            return Err(GitError::known("workspace_path_denied"));
        }
        let hooks_config = format!("core.hooksPath={}", git_path(&hooks)?);
        let mut command = CommandWrap::with_new("git", |command| {
            command.args([
                "--no-pager",
                "--no-optional-locks",
                "-c",
                &hooks_config,
                "-c",
                "core.fsmonitor=false",
            ]);
            #[cfg(windows)]
            command.args(["-c", "core.longpaths=true"]);
            command
                .args(args)
                .current_dir(cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .env_clear();
            for name in [
                "PATH",
                "SystemRoot",
                "WINDIR",
                "TEMP",
                "TMP",
                "LANG",
                "LC_ALL",
            ] {
                if let Some(value) = std::env::var_os(name) {
                    command.env(name, value);
                }
            }
            command
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("GIT_CONFIG_NOSYSTEM", "1");
            #[cfg(unix)]
            command.env("GIT_CONFIG_GLOBAL", "/dev/null");
            #[cfg(windows)]
            command.env("GIT_CONFIG_GLOBAL", "NUL");
        });
        command.wrap(KillOnDrop);
        #[cfg(unix)]
        command.wrap(ProcessGroup::leader());
        #[cfg(windows)]
        command.wrap(JobObject);
        let mut guard = ChildGuard(
            command
                .spawn()
                .map_err(|_| GitError::known("git_unavailable"))?,
        );
        let child = &mut guard.0;
        let mut stdout = child
            .stdout()
            .take()
            .ok_or(GitError::known("git_output_failed"))?;
        let mut stderr = child
            .stderr()
            .take()
            .ok_or(GitError::known("git_output_failed"))?;
        let output = async {
            let read = async {
                let mut out = Vec::new();
                let mut out_open = true;
                let mut err_open = true;
                let mut out_buffer = [0; 4096];
                let mut err_buffer = [0; 4096];
                let mut bytes = 0;
                while out_open || err_open {
                    let (is_stdout, count) = tokio::select! {
                        count = stdout.read(&mut out_buffer), if out_open => (true, count),
                        count = stderr.read(&mut err_buffer), if err_open => (false, count),
                    };
                    let count = count.map_err(|_| "git_output_failed")?;
                    if count == 0 {
                        if is_stdout {
                            out_open = false;
                        } else {
                            err_open = false;
                        }
                        continue;
                    }
                    bytes += count;
                    if bytes > OUTPUT_LIMIT {
                        return Err("git_output_limit");
                    }
                    if is_stdout {
                        out.extend_from_slice(&out_buffer[..count]);
                    }
                }
                let status = child.wait().await.map_err(|_| "git_wait_failed")?;
                if !status.success() {
                    return Err("git_failed");
                }
                Ok(out)
            };
            tokio::select! {
                result = read => result,
                _ = cancel.changed() => Err("cancelled"),
            }
        };
        let result = tokio::time::timeout(DEADLINE, output)
            .await
            .unwrap_or(Err("git_timeout"));
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
        result.map_err(|code| GitError {
            code,
            effects_unknown: mutation,
        })
    }

    pub async fn inspect(&self, path: &str) -> Result<Repository, GitError> {
        let (_sender, mut cancel) = watch::channel(false);
        self.inspect_with_cancel(path, &mut cancel).await
    }

    async fn inspect_with_cancel(
        &self,
        path: &str,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<Repository, GitError> {
        if !Path::new(path).is_absolute() || path.chars().any(char::is_control) {
            return Err(GitError::known("invalid_project"));
        }
        let path = tokio::fs::canonicalize(path)
            .await
            .map_err(|_| GitError::known("invalid_project"))?;
        let root = line(
            self.run(&path, &["rev-parse", "--show-toplevel"], false, cancel)
                .await?,
        )?;
        let common = line(
            self.run(
                &path,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
                false,
                cancel,
            )
            .await?,
        )?;
        Ok(Repository {
            path: canonical_path(&root).await?,
            common_dir: canonical_path(&common).await?,
        })
    }

    async fn verify(
        &self,
        repo: &Repository,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<(), GitError> {
        let found = self.inspect_with_cancel(&repo.path, cancel).await?;
        if found.path != repo.path || found.common_dir != repo.common_dir {
            return Err(GitError::known("project_changed"));
        }
        Ok(())
    }

    pub async fn resolve(&self, repo: &Repository, base: Option<&str>) -> Result<String, GitError> {
        let (_sender, mut cancel) = watch::channel(false);
        self.verify(repo, &mut cancel).await?;
        let base = base.unwrap_or("HEAD");
        if base.is_empty()
            || base.len() > 256
            || base.starts_with('-')
            || base.chars().any(char::is_control)
        {
            return Err(GitError::known("invalid_base"));
        }
        let expression = format!("{base}^{{commit}}");
        let commit = line(
            self.run(
                Path::new(&repo.path),
                &["rev-parse", "--verify", "--end-of-options", &expression],
                false,
                &mut cancel,
            )
            .await
            .map_err(|_| GitError::known("invalid_base"))?,
        )?;
        if !valid_commit(&commit) {
            return Err(GitError::known("invalid_base"));
        }
        Ok(commit)
    }

    pub async fn allocate(
        &self,
        repo: &Repository,
        path: &str,
        commit: &str,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<(), GitError> {
        let _mutation = tokio::select! {
            lock = self.mutations.lock() => lock,
            _ = cancel.changed() => return Err(GitError::known("cancelled")),
        };
        self.verify(repo, cancel).await?;
        let path = Path::new(path);
        if !valid_commit(commit) || !path.starts_with(self.data_dir.join("workspaces")) {
            return Err(GitError::known("workspace_path_denied"));
        }
        let parent = path
            .parent()
            .ok_or(GitError::known("workspace_path_denied"))?;
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|_| GitError::known("workspace_path_denied"))?;
        if tokio::fs::canonicalize(parent).await.ok().as_deref() != Some(parent)
            || tokio::fs::symlink_metadata(path).await.is_ok()
        {
            return Err(GitError::known("workspace_path_denied"));
        }
        let destination = git_path(path)?;
        self.run(
            Path::new(&repo.path),
            &["worktree", "add", "--detach", "--", &destination, commit],
            true,
            cancel,
        )
        .await?;
        // Never accept a replaced path or a different checkout as this workspace.
        self.verify_workspace(repo, path, cancel)
            .await
            .map_err(|mut error| {
                error.effects_unknown = true;
                error
            })?;
        Ok(())
    }

    pub async fn validate_workspace(
        &self,
        repo: &Repository,
        path: &str,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<(), GitError> {
        self.verify_workspace(repo, Path::new(path), cancel).await
    }

    async fn verify_workspace(
        &self,
        repo: &Repository,
        path: &Path,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<(), GitError> {
        self.verify(repo, cancel).await?;
        if !path.starts_with(self.data_dir.join("workspaces"))
            || tokio::fs::canonicalize(path).await.ok().as_deref() != Some(path)
        {
            return Err(GitError::known("workspace_path_denied"));
        }
        let found = self
            .inspect_with_cancel(
                path.to_str()
                    .ok_or(GitError::known("workspace_path_denied"))?,
                cancel,
            )
            .await?;
        if Path::new(&found.path) != path || found.common_dir != repo.common_dir {
            return Err(GitError::known("workspace_path_denied"));
        }
        Ok(())
    }

    pub async fn diff(
        &self,
        repo: &Repository,
        path: &str,
        base: &str,
    ) -> Result<WorkspaceDiff, GitError> {
        let (_sender, mut cancel) = watch::channel(false);
        self.diff_with_cancel(repo, path, base, &mut cancel).await
    }

    async fn diff_with_cancel(
        &self,
        repo: &Repository,
        path: &str,
        base: &str,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<WorkspaceDiff, GitError> {
        self.verify_workspace(repo, Path::new(path), cancel).await?;
        if !valid_commit(base) {
            return Err(GitError::known("invalid_base"));
        }
        let head_commit = line(
            self.run(
                Path::new(path),
                &["rev-parse", "--verify", "HEAD"],
                false,
                cancel,
            )
            .await?,
        )?;
        let status = self
            .run(
                Path::new(path),
                &[
                    "status",
                    "--porcelain=v1",
                    "--untracked-files=all",
                    "--ignored=matching",
                ],
                false,
                cancel,
            )
            .await?;
        let patch = self
            .run(
                Path::new(path),
                &[
                    "diff",
                    "--no-ext-diff",
                    "--no-textconv",
                    "--no-color",
                    base,
                    "--",
                ],
                false,
                cancel,
            )
            .await?;
        Ok(WorkspaceDiff {
            head_commit,
            patch: String::from_utf8_lossy(&patch).into_owned(),
            status: String::from_utf8_lossy(&status).into_owned(),
        })
    }

    pub async fn remove(
        &self,
        repo: &Repository,
        path: &str,
        base: &str,
        cancel: &mut watch::Receiver<bool>,
    ) -> Result<(), GitError> {
        let _mutation = tokio::select! {
            lock = self.mutations.lock() => lock,
            _ = cancel.changed() => return Err(GitError::known("cancelled")),
        };
        let diff = self.diff_with_cancel(repo, path, base, cancel).await?;
        if !diff.status.is_empty() {
            return Err(GitError::known("workspace_dirty"));
        }
        if diff.head_commit != base {
            return Err(GitError::known("workspace_has_commits"));
        }
        let destination = git_path(Path::new(path))?;
        self.run(
            Path::new(&repo.path),
            &["worktree", "remove", "--", &destination],
            true,
            cancel,
        )
        .await?;
        Ok(())
    }
}

// Keep canonical paths for identity and containment checks. Git for Windows
// rejects the question mark in Rust's verbatim paths when creating a worktree;
// translate only arguments passed to Git, which manages long paths itself.
#[cfg(not(windows))]
fn git_path(path: &Path) -> Result<String, GitError> {
    path.to_str()
        .map(str::to_owned)
        .ok_or(GitError::known("workspace_path_denied"))
}

#[cfg(windows)]
fn git_path(path: &Path) -> Result<String, GitError> {
    use std::path::{Component, Prefix};

    let value = path
        .to_str()
        .ok_or(GitError::known("workspace_path_denied"))?;
    match path.components().next() {
        Some(Component::Prefix(prefix)) => match prefix.kind() {
            Prefix::VerbatimDisk(_) => Ok(value[4..].replace('\\', "/")),
            Prefix::VerbatimUNC(_, _) => Ok(format!("//{}", value[8..].replace('\\', "/"))),
            Prefix::Verbatim(_) | Prefix::DeviceNS(_) => {
                Err(GitError::known("workspace_path_denied"))
            }
            _ => Ok(value.replace('\\', "/")),
        },
        _ => Ok(value.replace('\\', "/")),
    }
}

async fn canonical_path(path: &str) -> Result<String, GitError> {
    tokio::fs::canonicalize(path)
        .await
        .map_err(|_| GitError::known("invalid_project"))?
        .to_str()
        .map(str::to_owned)
        .ok_or(GitError::known("invalid_project"))
}
fn line(bytes: Vec<u8>) -> Result<String, GitError> {
    let value = String::from_utf8(bytes).map_err(|_| GitError::known("git_output_failed"))?;
    let value = value.trim_end_matches(['\r', '\n']);
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(GitError::known("git_output_failed"));
    }
    Ok(value.to_owned())
}
fn valid_commit(commit: &str) -> bool {
    matches!(commit.len(), 40 | 64) && commit.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn managed_worktree_supports_canonical_paths_beyond_windows_path_limit() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("repository with spaces");
        std::fs::create_dir(&source).unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.invalid",
                ])
                .args(args)
                .current_dir(&source)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        git(&["init"]);
        // Runtime Git configuration must override a repository's short-path
        // default, without changing the user's persisted configuration.
        git(&["config", "core.longpaths", "false"]);
        std::fs::write(source.join("file.txt"), "before\n").unwrap();
        git(&["add", "file.txt"]);
        git(&["commit", "-m", "base"]);

        let mut data = directory.path().join("data");
        for index in 0..6 {
            data = data.join(format!("{index}-{}end", "deep path ".repeat(4)));
        }
        std::fs::create_dir_all(&data).unwrap();
        let data = std::fs::canonicalize(data).unwrap();
        let service = GitService::new(data.clone());
        let repo = service.inspect(source.to_str().unwrap()).await.unwrap();
        let base = service.resolve(&repo, None).await.unwrap();
        let workspace = data.join("workspaces").join("project").join("workspace");
        let path = workspace.to_str().unwrap();
        assert!(path.len() > 260);
        let (_sender, mut cancel) = watch::channel(false);
        service
            .allocate(&repo, path, &base, &mut cancel)
            .await
            .unwrap();
        assert_eq!(std::fs::canonicalize(&workspace).unwrap(), workspace);
        assert_eq!(
            std::fs::read_to_string(workspace.join("file.txt")).unwrap(),
            "before\n"
        );
        std::fs::write(workspace.join("file.txt"), "after\n").unwrap();
        let diff = service.diff(&repo, path, &base).await.unwrap();
        assert_eq!(diff.head_commit, base);
        assert!(diff.patch.contains("+after"));
        assert!(diff.patch.contains("-before"));
        let error = service
            .remove(&repo, path, &base, &mut cancel)
            .await
            .unwrap_err();
        assert_eq!(error.code, "workspace_dirty");
        assert!(!error.effects_unknown);
        assert!(workspace.is_dir());
        assert_eq!(
            std::fs::read_to_string(source.join("file.txt")).unwrap(),
            "before\n"
        );
        std::fs::write(workspace.join("file.txt"), "before\n").unwrap();
        service
            .remove(&repo, path, &base, &mut cancel)
            .await
            .unwrap();
        assert!(!workspace.exists());
    }
}
