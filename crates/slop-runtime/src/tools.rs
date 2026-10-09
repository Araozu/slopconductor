//! Explicit workspace capabilities and bounded native tool supervision.
//! A workspace limits file-tool authority; shell access is not an OS sandbox.

use std::{
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use cap_std::fs::{Dir, OpenOptions};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::providers::inference::ToolDefinition;

pub const NAMES: &[&str] = &[
    "list_files",
    "read_file",
    "write_file",
    "apply_patch",
    "shell",
];
pub const MAX_FILE_BYTES: usize = 1024 * 1024;
pub const PREVIEW_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePolicy {
    pub root: String,
    pub allowed_tools: Vec<String>,
    pub shell_timeout_ms: u64,
    pub max_output_bytes: usize,
    pub max_tool_calls: u32,
    pub max_model_requests: u32,
}

impl WorkspacePolicy {
    pub fn validate(&self) -> std::io::Result<()> {
        if !Path::new(&self.root).is_absolute()
            || self.allowed_tools.is_empty()
            || self.allowed_tools.len() > NAMES.len()
            || self
                .allowed_tools
                .iter()
                .any(|name| !NAMES.contains(&name.as_str()))
            || self
                .allowed_tools
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != self.allowed_tools.len()
            || !(1..=300_000).contains(&self.shell_timeout_ms)
            || !(1024..=8 * 1024 * 1024).contains(&self.max_output_bytes)
            || !(1..=128).contains(&self.max_tool_calls)
            || !(1..=64).contains(&self.max_model_requests)
        {
            return Err(std::io::Error::other("invalid workspace policy"));
        }
        Ok(())
    }

    pub fn canonicalize(mut self) -> std::io::Result<Self> {
        self.validate()?;
        let root = std::fs::canonicalize(&self.root)?;
        if !root.is_dir() {
            return Err(std::io::Error::other("workspace is not a directory"));
        }
        self.root = root
            .to_str()
            .ok_or_else(|| std::io::Error::other("workspace path is not UTF-8"))?
            .to_owned();
        Ok(self)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Artifact {
    pub id: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub media_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolOutcome {
    pub status: String,
    pub output: String,
    pub artifacts: Vec<Artifact>,
    pub error_code: Option<String>,
    pub effects_unknown: bool,
}

impl ToolOutcome {
    pub fn failed(code: &str, effects_unknown: bool) -> Self {
        Self {
            status: "failed".into(),
            output: format!("Tool failed: {code}."),
            artifacts: Vec::new(),
            error_code: Some(code.into()),
            effects_unknown,
        }
    }
}

// Dropping an execution future must target the wrapper's process group/job,
// including when shutdown aborts the task before normal cleanup completes.
struct SupervisedChild(Box<dyn process_wrap::tokio::ChildWrapper>);
impl Drop for SupervisedChild {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

pub struct ToolService {
    artifact_dir: Option<PathBuf>,
    slots: Arc<tokio::sync::Semaphore>,
}

impl ToolService {
    pub fn disabled() -> Self {
        Self {
            artifact_dir: None,
            slots: Arc::new(tokio::sync::Semaphore::new(4)),
        }
    }

    pub fn new(artifact_dir: PathBuf) -> std::io::Result<Self> {
        std::fs::create_dir_all(&artifact_dir)?;
        if std::fs::symlink_metadata(&artifact_dir)?
            .file_type()
            .is_symlink()
        {
            return Err(std::io::Error::other(
                "artifact directory cannot be a symlink",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&artifact_dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(Self {
            artifact_dir: Some(std::fs::canonicalize(artifact_dir)?),
            slots: Arc::new(tokio::sync::Semaphore::new(4)),
        })
    }

    pub fn artifact_path(&self, id: &str) -> Option<PathBuf> {
        if id.len() != 64
            || !id
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return None;
        }
        self.artifact_dir.as_ref().map(|dir| dir.join(id))
    }

    pub async fn run(
        &self,
        policy: WorkspacePolicy,
        name: &str,
        arguments: Value,
        mut cancel: tokio::sync::watch::Receiver<bool>,
        on_output: &mut (dyn FnMut(&str, &str) + Send),
    ) -> ToolOutcome {
        if policy.validate().is_err() || !policy.allowed_tools.iter().any(|n| n == name) {
            return ToolOutcome::failed("tool_not_allowed", false);
        }
        let permit = tokio::select! {
            permit=self.slots.clone().acquire_owned()=>match permit{Ok(p)=>p,Err(_)=>return ToolOutcome::failed("tool_unavailable",false)},
            _=cancel.changed()=>return ToolOutcome::failed("cancelled",false),
        };
        let _permit = Arc::new(permit);
        if *cancel.borrow() {
            return ToolOutcome::failed("cancelled", false);
        }
        if name == "shell" {
            return self.shell(&policy, arguments, &mut cancel, on_output).await;
        }
        let output_limit = policy.max_output_bytes;
        let writes = matches!(name, "write_file" | "apply_patch");
        let name = name.to_owned();
        let blocking_permit = Arc::clone(&_permit);
        let result = tokio::task::spawn_blocking(move || {
            let _permit = blocking_permit;
            file_tool(&policy, &name, arguments)
        })
        .await;
        match result {
            Ok(Ok(output)) => {
                let mut bytes = output.into_bytes();
                let exceeded = bytes.len() > output_limit;
                bytes.truncate(output_limit);
                self.complete_output(
                    bytes,
                    "text/plain; charset=utf-8",
                    if exceeded {
                        Some("tool_output_limit")
                    } else {
                        None
                    },
                    false,
                )
                .await
            }
            Ok(Err(code)) => ToolOutcome::failed(code, writes && code == "file_write_failed"),
            Err(_) => ToolOutcome::failed("tool_worker_failed", true),
        }
    }

    async fn complete_output(
        &self,
        bytes: Vec<u8>,
        media_type: &str,
        error_code: Option<&str>,
        effects_unknown: bool,
    ) -> ToolOutcome {
        let preview =
            String::from_utf8_lossy(&bytes[..bytes.len().min(PREVIEW_BYTES)]).into_owned();
        let mut artifacts = Vec::new();
        if bytes.len() > PREVIEW_BYTES {
            match self.save_artifact(&bytes, media_type).await {
                Ok(artifact) => artifacts.push(artifact),
                Err(_) => return ToolOutcome::failed("artifact_storage_failed", effects_unknown),
            }
        }
        ToolOutcome {
            status: if error_code.is_some() {
                "failed"
            } else {
                "completed"
            }
            .into(),
            output: preview,
            artifacts,
            error_code: error_code.map(str::to_owned),
            effects_unknown,
        }
    }

    async fn save_artifact(&self, bytes: &[u8], media_type: &str) -> std::io::Result<Artifact> {
        let dir = self
            .artifact_dir
            .as_ref()
            .ok_or_else(|| std::io::Error::other("artifact storage unavailable"))?;
        let digest = format!("{:x}", Sha256::digest(bytes));
        let temp = tempfile::NamedTempFile::new_in(dir)?;
        let mut file = tokio::fs::File::from_std(temp.reopen()?);
        file.write_all(bytes).await?;
        file.sync_all().await?;
        drop(file);
        let destination = dir.join(&digest);
        match temp.persist_noclobber(&destination) {
            Ok(file) => {
                file.sync_all()?;
            }
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                let existing = tokio::fs::read(&destination).await?;
                if existing != bytes {
                    return Err(std::io::Error::other("artifact checksum conflict"));
                }
            }
            Err(error) => return Err(error.error),
        }
        #[cfg(unix)]
        {
            tokio::fs::File::open(dir).await?.sync_all().await?;
        }
        Ok(Artifact {
            id: digest.clone(),
            sha256: digest,
            size_bytes: bytes.len() as u64,
            media_type: media_type.into(),
        })
    }

    async fn shell(
        &self,
        policy: &WorkspacePolicy,
        arguments: Value,
        cancel: &mut tokio::sync::watch::Receiver<bool>,
        on_output: &mut (dyn FnMut(&str, &str) + Send),
    ) -> ToolOutcome {
        let args: ShellArgs = match serde_json::from_value(arguments) {
            Ok(args) => args,
            Err(_) => return ToolOutcome::failed("invalid_arguments", false),
        };
        if args.command.is_empty() || args.command.len() > 64 * 1024 || args.command.contains('\0')
        {
            return ToolOutcome::failed("invalid_arguments", false);
        }
        let relative = match relative_path(&args.cwd) {
            Ok(path) => path,
            Err(code) => return ToolOutcome::failed(code, false),
        };
        let cwd = match std::fs::canonicalize(Path::new(&policy.root).join(relative)) {
            Ok(cwd) if cwd.starts_with(&policy.root) && cwd.is_dir() => cwd,
            _ => return ToolOutcome::failed("workspace_path_denied", false),
        };
        use process_wrap::tokio::*;
        #[cfg(unix)]
        let mut command = CommandWrap::with_new("/bin/sh", |c| {
            c.args(["-c", &args.command]);
        });
        #[cfg(windows)]
        let mut command = CommandWrap::with_new("powershell.exe", |c| {
            c.args(["-NoProfile", "-NonInteractive", "-Command", &args.command]);
        });
        command
            .command_mut()
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
                command.command_mut().env(name, value);
            }
        }
        command.wrap(KillOnDrop);
        #[cfg(unix)]
        command.wrap(ProcessGroup::leader());
        #[cfg(windows)]
        command.wrap(JobObject);
        let child = match command.spawn() {
            Ok(child) => child,
            Err(_) => return ToolOutcome::failed("process_spawn_failed", false),
        };
        let mut guard = SupervisedChild(child);
        let child = &mut guard.0;
        let mut stdout = child.stdout().take().expect("piped stdout");
        let mut stderr = child.stderr().take().expect("piped stderr");
        let deadline = tokio::time::sleep(Duration::from_millis(policy.shell_timeout_ms));
        tokio::pin!(deadline);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut out_open = true;
        let mut err_open = true;
        let mut out_buffer = [0u8; 4096];
        let mut err_buffer = [0u8; 4096];
        let mut failure = None;
        while out_open || err_open {
            let (stream, read) = tokio::select! {
                read=stdout.read(&mut out_buffer),if out_open=>("stdout",read),
                read=stderr.read(&mut err_buffer),if err_open=>("stderr",read),
                _=&mut deadline=>{failure=Some("tool_timeout");break;},
                _=cancel.changed()=>{failure=Some("cancelled");break;},
            };
            let n = match read {
                Ok(n) => n,
                Err(_) => {
                    failure = Some("process_output_failed");
                    break;
                }
            };
            if n == 0 {
                if stream == "stdout" {
                    out_open = false;
                } else {
                    err_open = false;
                }
                continue;
            }
            let buffer = if stream == "stdout" {
                &out_buffer[..n]
            } else {
                &err_buffer[..n]
            };
            let remaining = policy
                .max_output_bytes
                .saturating_sub(out.len() + err.len());
            let chunk = &buffer[..buffer.len().min(remaining)];
            if stream == "stdout" {
                out.extend_from_slice(chunk);
            } else {
                err.extend_from_slice(chunk);
            }
            on_output(stream, &String::from_utf8_lossy(chunk));
            if chunk.len() != buffer.len() {
                failure = Some("tool_output_limit");
                break;
            }
        }
        let mut effects_unknown = failure.is_some();
        let status = if failure.is_some() {
            if child.start_kill().is_err() {
                effects_unknown = true;
            }
            tokio::time::timeout(Duration::from_secs(2), child.wait())
                .await
                .ok()
                .and_then(std::result::Result::ok)
        } else {
            tokio::select! {
                status=child.wait()=>status.ok(),
                _=&mut deadline=>{failure=Some("tool_timeout");effects_unknown=true;let _=child.start_kill();tokio::time::timeout(Duration::from_secs(2),child.wait()).await.ok().and_then(std::result::Result::ok)},
                _=cancel.changed()=>{failure=Some("cancelled");effects_unknown=true;let _=child.start_kill();tokio::time::timeout(Duration::from_secs(2),child.wait()).await.ok().and_then(std::result::Result::ok)},
            }
        };
        // Even a successful leader may leave descendants with closed pipes.
        // Supervised shell invocations do not grant background-process lifetime.
        let _ = child.start_kill();
        if status.is_none() {
            failure = Some("process_wait_failed");
            effects_unknown = true;
        } else if status.is_some_and(|status| !status.success()) && failure.is_none() {
            failure = Some("process_exit_failed");
        }
        let output=json!({"exit_code":status.and_then(|s|s.code()),"stdout":String::from_utf8_lossy(&out),"stderr":String::from_utf8_lossy(&err)}).to_string();
        self.complete_output(
            output.into_bytes(),
            "application/json",
            failure,
            effects_unknown,
        )
        .await
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellArgs {
    command: String,
    #[serde(default = "dot")]
    cwd: String,
}
fn dot() -> String {
    ".".into()
}
fn read_limit() -> usize {
    64 * 1024
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    path: String,
    #[serde(default)]
    offset: usize,
    #[serde(default = "read_limit")]
    limit: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    #[serde(default = "dot")]
    path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    content: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchArgs {
    path: String,
    old_text: String,
    new_text: String,
}

fn relative_path(value: &str) -> std::result::Result<&Path, &'static str> {
    let path = Path::new(value);
    if value.is_empty()
        || value.contains('\0')
        || path.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err("workspace_path_denied");
    }
    Ok(path)
}
fn read_bounded(dir: &Dir, path: &Path) -> std::result::Result<String, &'static str> {
    let mut options = cap_std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt;
        options.custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32);
    }
    let mut file = dir
        .open_with(path, &options)
        .map_err(|_| "file_open_failed")?;
    if !file
        .metadata()
        .map_err(|_| "file_metadata_failed")?
        .is_file()
    {
        return Err("not_regular_file");
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take((MAX_FILE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "file_read_failed")?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err("file_size_limit");
    }
    String::from_utf8(bytes).map_err(|_| "file_not_utf8")
}
fn file_tool(
    policy: &WorkspacePolicy,
    name: &str,
    arguments: Value,
) -> std::result::Result<String, &'static str> {
    let dir = Dir::open_ambient_dir(&policy.root, cap_std::ambient_authority())
        .map_err(|_| "workspace_unavailable")?;
    match name {
        "read_file" => {
            let args: ReadArgs =
                serde_json::from_value(arguments).map_err(|_| "invalid_arguments")?;
            if args.limit == 0 || args.limit > 256 * 1024 {
                return Err("invalid_arguments");
            }
            let text = read_bounded(&dir, relative_path(&args.path)?)?;
            if args.offset > text.len() || !text.is_char_boundary(args.offset) {
                return Err("invalid_offset");
            }
            let mut end = args.offset.saturating_add(args.limit).min(text.len());
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            Ok(json!({"path":args.path,"offset":args.offset,"next_offset":if end<text.len(){Some(end)}else{None},"text":&text[args.offset..end]}).to_string())
        }
        "list_files" => {
            let args: ListArgs =
                serde_json::from_value(arguments).map_err(|_| "invalid_arguments")?;
            let subdir = dir
                .open_dir(relative_path(&args.path)?)
                .map_err(|_| "directory_open_failed")?;
            let mut names = Vec::new();
            for entry in subdir.entries().map_err(|_| "directory_read_failed")? {
                if names.len() >= 512 {
                    return Err("directory_entry_limit");
                }
                let entry = entry.map_err(|_| "directory_read_failed")?;
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
            names.sort();
            Ok(json!({"path":args.path,"entries":names}).to_string())
        }
        "write_file" => {
            let args: WriteArgs =
                serde_json::from_value(arguments).map_err(|_| "invalid_arguments")?;
            if args.content.len() > MAX_FILE_BYTES {
                return Err("file_size_limit");
            }
            let path = relative_path(&args.path)?;
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            let mut file = dir
                .open_with(path, &options)
                .map_err(|_| "file_create_failed")?;
            file.write_all(args.content.as_bytes())
                .and_then(|_| file.sync_all())
                .map_err(|_| "file_write_failed")?;
            Ok(json!({"path":args.path,"bytes_written":args.content.len()}).to_string())
        }
        "apply_patch" => {
            let args: PatchArgs =
                serde_json::from_value(arguments).map_err(|_| "invalid_arguments")?;
            if args.old_text.is_empty() {
                return Err("invalid_arguments");
            }
            let path = relative_path(&args.path)?;
            let old = read_bounded(&dir, path)?;
            if old.matches(&args.old_text).count() != 1 {
                return Err("patch_precondition_failed");
            }
            let new = old.replacen(&args.old_text, &args.new_text, 1);
            if new.len() > MAX_FILE_BYTES {
                return Err("file_size_limit");
            }
            let mut random = [0u8; 16];
            getrandom::fill(&mut random).map_err(|_| "entropy_unavailable")?;
            let temp = path.with_file_name(format!(".slop-{:x}.tmp", u128::from_le_bytes(random)));
            let result = (|| {
                let mut options = OpenOptions::new();
                options.write(true).create_new(true);
                let mut file = dir
                    .open_with(&temp, &options)
                    .map_err(|_| "file_create_failed")?;
                file.set_permissions(
                    dir.metadata(path)
                        .map_err(|_| "file_metadata_failed")?
                        .permissions(),
                )
                .map_err(|_| "file_permissions_failed")?;
                file.write_all(new.as_bytes())
                    .and_then(|_| file.sync_all())
                    .map_err(|_| "file_write_failed")?;
                drop(file);
                if read_bounded(&dir, path)? != old {
                    return Err("patch_precondition_failed");
                }
                dir.rename(&temp, &dir, path)
                    .map_err(|_| "file_replace_failed")?;
                Ok(json!({"path":args.path,"bytes_written":new.len()}).to_string())
            })();
            if result.is_err() {
                let _ = dir.remove_file(&temp);
            }
            result
        }
        _ => Err("unknown_tool"),
    }
}

pub fn definitions() -> Vec<ToolDefinition> {
    let tool = |name: &str, description: &str, properties: Value, required: Vec<&str>| {
        ToolDefinition {
            name: name.into(),
            description: description.into(),
            parameters: json!({"type":"object","properties":properties,"required":required,"additionalProperties":false}),
        }
    };
    vec![
        tool(
            "list_files",
            "List at most 512 names in a workspace-relative directory. Parent traversal and escapes are rejected. Large directories return an explicit limit error.",
            json!({"path":{"type":"string"}}),
            vec![],
        ),
        tool(
            "read_file",
            "Read a bounded UTF-8 byte range of a workspace file up to 1 MiB. Offset must fall on a UTF-8 boundary; next_offset indicates remaining content. Limit defaults to 65536 and cannot exceed 262144.",
            json!({"path":{"type":"string"},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":262144}}),
            vec!["path"],
        ),
        tool(
            "write_file",
            "Create a new UTF-8 file in the workspace. Existing files are never overwritten; use apply_patch to modify them. Parent directories must already exist.",
            json!({"path":{"type":"string"},"content":{"type":"string"}}),
            vec!["path", "content"],
        ),
        tool(
            "apply_patch",
            "Replace exactly one occurrence of old_text in a workspace file with new_text. The old text must be nonempty and uniquely match. Replacement uses an atomic rename and preserves permissions; ambiguous or stale patches fail.",
            json!({"path":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"}}),
            vec!["path", "old_text", "new_text"],
        ),
        tool(
            "shell",
            "Execute a shell command with a workspace-relative working directory, a filtered environment, a deadline, and bounded stdout/stderr. Linux uses /bin/sh; Windows uses PowerShell without profiles. Shell access is not sandboxed and may cause external changes. Background descendants are terminated when the invocation ends.",
            json!({"command":{"type":"string"},"cwd":{"type":"string"}}),
            vec!["command"],
        ),
    ]
}

pub fn validate_arguments(name: &str, arguments: &Value) -> bool {
    match name {
        "list_files" => serde_json::from_value::<ListArgs>(arguments.clone()).is_ok(),
        "read_file" => serde_json::from_value::<ReadArgs>(arguments.clone()).is_ok(),
        "write_file" => serde_json::from_value::<WriteArgs>(arguments.clone()).is_ok(),
        "apply_patch" => serde_json::from_value::<PatchArgs>(arguments.clone()).is_ok(),
        "shell" => serde_json::from_value::<ShellArgs>(arguments.clone()).is_ok(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn policy(root: &Path) -> WorkspacePolicy {
        WorkspacePolicy {
            root: root.to_str().unwrap().into(),
            allowed_tools: NAMES.iter().map(|s| (*s).into()).collect(),
            shell_timeout_ms: 2000,
            max_output_bytes: 1024 * 1024,
            max_tool_calls: 32,
            max_model_requests: 16,
        }
    }
    #[test]
    fn patches_require_a_unique_precondition_and_writes_cannot_clobber_files() {
        let root = tempfile::tempdir().unwrap();
        let policy = policy(root.path());
        let path = root.path().join("file.txt");
        std::fs::write(&path, "old old").unwrap();
        assert_eq!(
            file_tool(
                &policy,
                "apply_patch",
                json!({"path":"file.txt","old_text":"old","new_text":"new"})
            ),
            Err("patch_precondition_failed")
        );
        assert_eq!(
            file_tool(
                &policy,
                "write_file",
                json!({"path":"file.txt","content":"new"})
            ),
            Err("file_create_failed")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old old");
        file_tool(
            &policy,
            "apply_patch",
            json!({"path":"file.txt","old_text":"old old","new_text":"new"}),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        assert!(file_tool(&policy, "read_file", json!({"path":"../outside"})).is_err());
        assert!(!validate_arguments(
            "shell",
            &json!({"command":"echo","environment":{"SECRET":"value"}})
        ));
    }
    #[cfg(unix)]
    #[test]
    fn directory_handles_prevent_symlink_escapes_and_patches_preserve_permissions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "outside").unwrap();
        symlink(outside.path(), root.path().join("escape")).unwrap();
        let policy = policy(root.path());
        assert!(file_tool(&policy, "read_file", json!({"path":"escape/secret"})).is_err());
        assert!(
            file_tool(
                &policy,
                "write_file",
                json!({"path":"escape/new","content":"bad"})
            )
            .is_err()
        );
        assert!(!outside.path().join("new").exists());
        std::fs::write(root.path().join("file"), "old").unwrap();
        std::fs::set_permissions(
            root.path().join("file"),
            std::fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        file_tool(
            &policy,
            "apply_patch",
            json!({"path":"file","old_text":"old","new_text":"new"}),
        )
        .unwrap();
        assert_eq!(
            std::fs::metadata(root.path().join("file"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
    }
    #[tokio::test]
    async fn oversized_tool_output_is_capped_and_referenced_by_a_durable_artifact() {
        let root = tempfile::tempdir().unwrap();
        let service = ToolService::new(root.path().join("artifacts")).unwrap();
        std::fs::write(root.path().join("large"), "x".repeat(32 * 1024)).unwrap();
        let (_sender, cancel) = tokio::sync::watch::channel(false);
        let result = service
            .run(
                policy(root.path()),
                "read_file",
                json!({"path":"large"}),
                cancel,
                &mut |_, _| {},
            )
            .await;
        assert_eq!(result.status, "completed");
        assert!(result.output.len() <= PREVIEW_BYTES);
        assert_eq!(result.artifacts.len(), 1);
        let artifact = &result.artifacts[0];
        let bytes = std::fs::read(service.artifact_path(&artifact.id).unwrap()).unwrap();
        assert_eq!(bytes.len() as u64, artifact.size_bytes);
        assert_eq!(format!("{:x}", Sha256::digest(bytes)), artifact.sha256);
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn shell_filters_credentials_caps_noisy_output_and_kills_descendants_on_cancel() {
        let root = tempfile::tempdir().unwrap();
        let service = Arc::new(ToolService::new(root.path().join("artifacts")).unwrap());
        let (_sender, cancel) = tokio::sync::watch::channel(false);
        let result = service
            .run(
                policy(root.path()),
                "shell",
                json!({"command":"printf '%s' \"${OPENCODE_GO_API_KEY-unset}\""}),
                cancel,
                &mut |_, _| {},
            )
            .await;
        assert_eq!(result.status, "completed");
        assert!(result.output.contains("unset"));
        let mut cap = policy(root.path());
        cap.max_output_bytes = 1024;
        let (_sender, cancel) = tokio::sync::watch::channel(false);
        let mut streamed = 0;
        let result = service
            .run(
                cap,
                "shell",
                json!({"command":"yes noisy"}),
                cancel,
                &mut |_, text| streamed += text.len(),
            )
            .await;
        assert_eq!(result.error_code.as_deref(), Some("tool_output_limit"));
        assert!(streamed <= 1024);
        assert!(result.effects_unknown);
        let mut p = policy(root.path());
        p.shell_timeout_ms = 30_000;
        let (sender, cancel) = tokio::sync::watch::channel(false);
        let running_service = Arc::clone(&service);
        let task = tokio::spawn(async move {
            running_service
                .run(
                    p,
                    "shell",
                    json!({"command":"sleep 30 & echo $! > child.pid; wait"}),
                    cancel,
                    &mut |_, _| {},
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !root.path().join("child.pid").exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let pid = std::fs::read_to_string(root.path().join("child.pid"))
            .unwrap()
            .trim()
            .to_owned();
        sender.send(true).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.error_code.as_deref(), Some("cancelled"));
        assert!(result.effects_unknown);
        #[cfg(target_os = "linux")]
        {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"));
            assert!(stat.is_err() || stat.unwrap().split_once(") ").unwrap().1.starts_with('Z'));
        }
    }
}
