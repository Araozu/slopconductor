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

pub const NAMES: &[&str] = &["read", "write", "edit", "bash"];
pub const MAX_FILE_BYTES: usize = 1024 * 1024;
pub const PREVIEW_BYTES: usize = 16 * 1024;
const MAX_READ_BYTES: usize = 256 * 1024;
const MAX_READ_LINES: usize = 2000;

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
        if !validate_arguments(name, &arguments) {
            return ToolOutcome::failed("invalid_arguments", false);
        }
        if name == "bash" {
            return self.bash(&policy, arguments, &mut cancel, on_output).await;
        }
        let output_limit = policy.max_output_bytes;
        let writes = matches!(name, "write" | "edit");
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
            Ok(Err(code)) => ToolOutcome::failed(
                code,
                writes
                    && matches!(
                        code,
                        "directory_create_failed"
                            | "file_write_failed"
                            | "file_create_failed"
                            | "file_replace_failed"
                    ),
            ),
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

    async fn bash(
        &self,
        policy: &WorkspacePolicy,
        arguments: Value,
        cancel: &mut tokio::sync::watch::Receiver<bool>,
        on_output: &mut (dyn FnMut(&str, &str) + Send),
    ) -> ToolOutcome {
        let args: BashArgs = match serde_json::from_value(arguments) {
            Ok(args) => args,
            Err(_) => return ToolOutcome::failed("invalid_arguments", false),
        };
        let cwd = match std::fs::canonicalize(&policy.root) {
            Ok(cwd) if cwd.starts_with(&policy.root) && cwd.is_dir() => cwd,
            _ => return ToolOutcome::failed("workspace_path_denied", false),
        };
        use process_wrap::tokio::*;
        #[cfg(unix)]
        let mut command = CommandWrap::with_new("/bin/bash", |c| {
            c.args(["--noprofile", "--norc", "-c", &args.command]);
        });
        #[cfg(windows)]
        let mut command = CommandWrap::with_new("bash.exe", |c| {
            c.args(["--noprofile", "--norc", "-c", &args.command]);
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
        let timeout = args
            .timeout
            .map(Duration::from_secs_f64)
            .unwrap_or(Duration::from_millis(policy.shell_timeout_ms))
            .min(Duration::from_millis(policy.shell_timeout_ms));
        let deadline = tokio::time::sleep(timeout);
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
struct BashArgs {
    command: String,
    timeout: Option<f64>,
}
fn first_line() -> usize {
    1
}
fn read_limit() -> usize {
    MAX_READ_LINES
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    path: String,
    #[serde(default = "first_line")]
    offset: usize,
    #[serde(default = "read_limit")]
    limit: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    content: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SingleEditArgs {
    path: String,
    old_text: String,
    new_text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Replacement {
    old_text: String,
    new_text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MultipleEditArgs {
    path: String,
    edits: Vec<Replacement>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum EditArgs {
    Single(SingleEditArgs),
    Multiple(MultipleEditArgs),
}

impl EditArgs {
    fn into_parts(self) -> (String, Vec<Replacement>) {
        match self {
            Self::Single(args) => (
                args.path,
                vec![Replacement {
                    old_text: args.old_text,
                    new_text: args.new_text,
                }],
            ),
            Self::Multiple(args) => (args.path, args.edits),
        }
    }
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
        "read" => {
            let args: ReadArgs =
                serde_json::from_value(arguments).map_err(|_| "invalid_arguments")?;
            if args.offset == 0 || args.limit == 0 {
                return Err("invalid_arguments");
            }
            let text = read_bounded(&dir, relative_path(&args.path)?)?;
            let total_lines = text.split('\n').count();
            if args.offset > total_lines {
                return Err("invalid_offset");
            }
            let mut selected = String::new();
            let mut count = 0;
            for line in text
                .split('\n')
                .skip(args.offset - 1)
                .take(args.limit.min(MAX_READ_LINES))
            {
                let separator = usize::from(count > 0);
                if selected.len() + separator + line.len() > MAX_READ_BYTES {
                    if count == 0 {
                        return Err("read_line_limit");
                    }
                    break;
                }
                if count > 0 {
                    selected.push('\n');
                }
                selected.push_str(line);
                count += 1;
            }
            let next = args.offset + count;
            Ok(json!({"path":args.path,"offset":args.offset,"next_offset":if next<=total_lines{Some(next)}else{None},"total_lines":total_lines,"text":selected}).to_string())
        }
        "write" => {
            let args: WriteArgs =
                serde_json::from_value(arguments).map_err(|_| "invalid_arguments")?;
            if args.content.len() > MAX_FILE_BYTES {
                return Err("file_size_limit");
            }
            let path = relative_path(&args.path)?;
            if path.file_name().is_none() {
                return Err("workspace_path_denied");
            }
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                dir.create_dir_all(parent)
                    .map_err(|_| "directory_create_failed")?;
            }
            replace_file(&dir, path, &args.content, None)?;
            Ok(json!({"path":args.path,"bytes_written":args.content.len()}).to_string())
        }
        "edit" => {
            let args: EditArgs =
                serde_json::from_value(arguments).map_err(|_| "invalid_arguments")?;
            let (path_string, edits) = args.into_parts();
            if edits.is_empty() || edits.iter().any(|edit| edit.old_text.is_empty()) {
                return Err("invalid_arguments");
            }
            let path = relative_path(&path_string)?;
            let old = read_bounded(&dir, path)?;
            let mut matches = Vec::new();
            let mut size = old.len();
            for edit in &edits {
                let start = old.find(&edit.old_text).ok_or("edit_precondition_failed")?;
                if old.rfind(&edit.old_text) != Some(start) {
                    return Err("edit_precondition_failed");
                }
                matches.push((start, start + edit.old_text.len(), &edit.new_text));
                size = size
                    .checked_add(edit.new_text.len())
                    .ok_or("file_size_limit")?;
            }
            matches.sort_by_key(|(start, _, _)| *start);
            let mut end = 0;
            for &(start, next_end, _) in &matches {
                if start < end {
                    return Err("edit_precondition_failed");
                }
                size -= next_end - start;
                end = next_end;
            }
            if size > MAX_FILE_BYTES {
                return Err("file_size_limit");
            }
            let mut new = String::with_capacity(size);
            end = 0;
            for (start, next_end, replacement) in matches {
                new.push_str(&old[end..start]);
                new.push_str(replacement);
                end = next_end;
            }
            new.push_str(&old[end..]);
            replace_file(&dir, path, &new, Some(&old))?;
            Ok(
                json!({"path":path_string,"bytes_written":new.len(),"edits_applied":edits.len()})
                    .to_string(),
            )
        }
        _ => Err("unknown_tool"),
    }
}

// Keep all replacement operations relative to an opened parent directory.
// Renaming a synced temporary file avoids partially overwriting the destination.
fn replace_file(
    dir: &Dir,
    path: &Path,
    content: &str,
    expected: Option<&str>,
) -> std::result::Result<(), &'static str> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let dir = dir.open_dir(parent).map_err(|_| "directory_open_failed")?;
    let name = path.file_name().ok_or("workspace_path_denied")?;
    let path = Path::new(name);
    let permissions = match dir.symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() => Some(metadata.permissions()),
        Ok(_) => return Err("not_regular_file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && expected.is_none() => None,
        Err(_) => return Err("file_metadata_failed"),
    };
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| "entropy_unavailable")?;
    let temp = PathBuf::from(format!(".slop-{:x}.tmp", u128::from_le_bytes(random)));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let mut file = dir
            .open_with(&temp, &options)
            .map_err(|_| "file_create_failed")?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)
                .map_err(|_| "file_permissions_failed")?;
        }
        file.write_all(content.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|_| "file_write_failed")?;
        drop(file);
        if let Some(expected) = expected
            && read_bounded(&dir, path)? != expected
        {
            return Err("edit_precondition_failed");
        }
        dir.rename(&temp, &dir, path)
            .map_err(|_| "file_replace_failed")
    })();
    if result.is_err() {
        let _ = dir.remove_file(&temp);
    }
    result
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
            "read",
            "Read a UTF-8 workspace-relative file up to 1 MiB. Offset is a 1-based line number (default 1); limit is a line count (default 2000). Returns at most 2000 complete lines and 256 KiB, with next_offset to continue. A single oversized line fails with read_line_limit. Absolute paths, parent traversal, and escapes are rejected. Images are not supported.",
            json!({"path":{"type":"string"},"offset":{"type":"integer","minimum":1},"limit":{"type":"integer","minimum":1}}),
            vec!["path"],
        ),
        tool(
            "write",
            "Create or overwrite a workspace-relative file with UTF-8 content up to 1 MiB. Creates parent directories as needed. Uses atomic replacement and preserves existing permissions. Use edit for targeted changes. Absolute paths, parent traversal, symlink destinations, and escapes are rejected.",
            json!({"path":{"type":"string"},"content":{"type":"string"}}),
            vec!["path", "content"],
        ),
        tool(
            "edit",
            "Edit a UTF-8 workspace-relative file using exact text replacement. Supply edits with oldText/newText for one or more replacements. Every oldText must be nonempty and uniquely match a non-overlapping region of the original file. A single top-level oldText/newText pair is also accepted. Replacement is atomic and preserves permissions. Ambiguous or stale edits fail; file size is capped at 1 MiB.",
            json!({"path":{"type":"string"},"oldText":{"type":"string","minLength":1},"newText":{"type":"string"},"edits":{"type":"array","minItems":1,"items":{"type":"object","properties":{"oldText":{"type":"string","minLength":1},"newText":{"type":"string"}},"required":["oldText","newText"],"additionalProperties":false}}}),
            vec!["path"],
        ),
        tool(
            "bash",
            "Execute a Bash command from the workspace root with no startup profiles, a filtered environment, and bounded stdout/stderr. Optional timeout is in seconds and can shorten the workspace deadline. Linux uses /bin/bash; Windows requires bash.exe on PATH. Commands may use cd for subdirectories. Bash is not sandboxed and may cause external changes. Background descendants are terminated when the invocation ends.",
            json!({"command":{"type":"string","minLength":1,"maxLength":65536},"timeout":{"type":"number","minimum":0.001,"maximum":300}}),
            vec!["command"],
        ),
    ].into_iter().map(|mut definition| {
        if definition.name == "edit" {
            definition.parameters["oneOf"] = json!([
                {"required":["oldText","newText"],"not":{"required":["edits"]}},
                {"required":["edits"],"not":{"anyOf":[{"required":["oldText"]},{"required":["newText"]}]}}
            ]);
        }
        definition
    }).collect()
}

pub fn validate_arguments(name: &str, arguments: &Value) -> bool {
    match name {
        "read" => serde_json::from_value::<ReadArgs>(arguments.clone())
            .is_ok_and(|args| args.offset > 0 && args.limit > 0),
        "write" => serde_json::from_value::<WriteArgs>(arguments.clone())
            .is_ok_and(|args| args.content.len() <= MAX_FILE_BYTES),
        "edit" => serde_json::from_value::<EditArgs>(arguments.clone()).is_ok_and(|args| {
            let (_, edits) = args.into_parts();
            !edits.is_empty() && edits.iter().all(|edit| !edit.old_text.is_empty())
        }),
        "bash" => serde_json::from_value::<BashArgs>(arguments.clone()).is_ok_and(|args| {
            !args.command.is_empty()
                && args.command.len() <= 64 * 1024
                && !args.command.contains('\0')
                && args
                    .timeout
                    .is_none_or(|timeout| timeout.is_finite() && (0.001..=300.0).contains(&timeout))
        }),
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
    fn edits_require_unique_nonoverlapping_preconditions() {
        let root = tempfile::tempdir().unwrap();
        let policy = policy(root.path());
        let path = root.path().join("file.txt");
        std::fs::write(&path, "old old").unwrap();
        assert_eq!(
            file_tool(
                &policy,
                "edit",
                json!({"path":"file.txt","oldText":"old","newText":"new"})
            ),
            Err("edit_precondition_failed")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "old old");
        file_tool(
            &policy,
            "edit",
            json!({"path":"file.txt","oldText":"old old","newText":"new"}),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        std::fs::write(&path, "alpha beta gamma").unwrap();
        file_tool(
            &policy,
            "edit",
            json!({"path":"file.txt","edits":[
                {"oldText":"gamma","newText":"alpha"},
                {"oldText":"alpha","newText":"gamma"}
            ]}),
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "gamma beta alpha");
        for arguments in [
            json!({"path":"file.txt","oldText":"missing","newText":"bad"}),
            json!({"path":"file.txt","edits":[
                {"oldText":"gamma beta","newText":"bad"},
                {"oldText":"beta alpha","newText":"bad"}
            ]}),
        ] {
            assert_eq!(
                file_tool(&policy, "edit", arguments),
                Err("edit_precondition_failed")
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "gamma beta alpha");
        }
        let dir = Dir::open_ambient_dir(root.path(), cap_std::ambient_authority()).unwrap();
        assert_eq!(
            replace_file(&dir, Path::new("file.txt"), "bad", Some("stale")),
            Err("edit_precondition_failed")
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        std::fs::write(&path, "aaa").unwrap();
        assert_eq!(
            file_tool(
                &policy,
                "edit",
                json!({"path":"file.txt","oldText":"aa","newText":"bad"})
            ),
            Err("edit_precondition_failed")
        );
        assert!(file_tool(&policy, "read", json!({"path":"../outside"})).is_err());
        assert!(!validate_arguments(
            "bash",
            &json!({"command":"echo","environment":{"SECRET":"value"}})
        ));
    }
    #[test]
    fn writes_create_parents_and_atomically_overwrite_bounded_files() {
        let root = tempfile::tempdir().unwrap();
        let policy = policy(root.path());
        let path = root.path().join("nested/deeper/file.txt");
        for content in ["original", "new 🦀", ""] {
            file_tool(
                &policy,
                "write",
                json!({"path":"nested/deeper/file.txt","content":content}),
            )
            .unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), content);
            assert_eq!(
                std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
                1
            );
        }
        assert_eq!(
            file_tool(
                &policy,
                "write",
                json!({"path":"too-large/file","content":"x".repeat(MAX_FILE_BYTES+1)})
            ),
            Err("file_size_limit")
        );
        assert!(!root.path().join("too-large").exists());
        assert_eq!(
            file_tool(&policy, "write", json!({"path":"nested","content":"bad"})),
            Err("not_regular_file")
        );
    }
    #[test]
    fn reads_page_complete_utf8_lines_with_one_based_offsets_and_bounds() {
        let root = tempfile::tempdir().unwrap();
        let policy = policy(root.path());
        std::fs::write(root.path().join("file"), "first\r\n🦀 second\r\nthird").unwrap();
        let output =
            file_tool(&policy, "read", json!({"path":"file","offset":2,"limit":1})).unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["text"], "🦀 second\r");
        assert_eq!(value["next_offset"], 3);
        let output = file_tool(&policy, "read", json!({"path":"file","offset":3})).unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["text"], "third");
        assert!(value["next_offset"].is_null());
        assert_eq!(
            file_tool(&policy, "read", json!({"path":"file","offset":4})),
            Err("invalid_offset")
        );
        std::fs::write(root.path().join("file"), "x\n".repeat(MAX_READ_LINES + 1)).unwrap();
        let output = file_tool(&policy, "read", json!({"path":"file","limit":usize::MAX})).unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["next_offset"], MAX_READ_LINES + 1);
        assert_eq!(
            value["text"].as_str().unwrap().split('\n').count(),
            MAX_READ_LINES
        );
        std::fs::write(
            root.path().join("file"),
            format!("{}\n🦀", "x".repeat(MAX_READ_BYTES)),
        )
        .unwrap();
        let output = file_tool(&policy, "read", json!({"path":"file"})).unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["next_offset"], 2);
        std::fs::write(root.path().join("file"), "x".repeat(MAX_READ_BYTES + 1)).unwrap();
        assert_eq!(
            file_tool(&policy, "read", json!({"path":"file"})),
            Err("read_line_limit")
        );
        std::fs::write(root.path().join("file"), "").unwrap();
        let output = file_tool(&policy, "read", json!({"path":"file"})).unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["text"], "");
        assert!(value["next_offset"].is_null());
        assert!(!validate_arguments(
            "read",
            &json!({"path":"file","offset":0})
        ));
        assert!(!validate_arguments(
            "read",
            &json!({"path":"file","limit":0})
        ));
        assert!(!validate_arguments(
            "edit",
            &json!({"path":"file","edits":[]})
        ));
    }
    #[cfg(unix)]
    #[test]
    fn directory_handles_prevent_symlink_escapes_and_mutations_preserve_permissions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "outside").unwrap();
        symlink(outside.path(), root.path().join("escape")).unwrap();
        let policy = policy(root.path());
        assert!(file_tool(&policy, "read", json!({"path":"escape/secret"})).is_err());
        assert!(
            file_tool(
                &policy,
                "write",
                json!({"path":"escape/new","content":"bad"})
            )
            .is_err()
        );
        assert!(!outside.path().join("new").exists());
        symlink(outside.path().join("secret"), root.path().join("link")).unwrap();
        assert_eq!(
            file_tool(&policy, "write", json!({"path":"link","content":"bad"})),
            Err("not_regular_file")
        );
        assert_eq!(
            file_tool(
                &policy,
                "edit",
                json!({"path":"escape/secret","oldText":"outside","newText":"bad"})
            ),
            Err("file_open_failed")
        );
        assert_eq!(
            std::fs::read_to_string(outside.path().join("secret")).unwrap(),
            "outside"
        );
        std::fs::write(root.path().join("file"), "old").unwrap();
        std::fs::set_permissions(
            root.path().join("file"),
            std::fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        file_tool(
            &policy,
            "edit",
            json!({"path":"file","oldText":"old","newText":"new"}),
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
        file_tool(
            &policy,
            "write",
            json!({"path":"file","content":"replacement"}),
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
                "read",
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
    async fn bash_honors_deadlines_and_reports_nonzero_exits_without_replay() {
        let root = tempfile::tempdir().unwrap();
        let service = ToolService::new(root.path().join("artifacts")).unwrap();
        for timeout in [0.0, -1.0, 301.0] {
            let (_sender, cancel) = tokio::sync::watch::channel(false);
            let result = service
                .run(
                    policy(root.path()),
                    "bash",
                    json!({"command":"touch invalid","timeout":timeout}),
                    cancel,
                    &mut |_, _| {},
                )
                .await;
            assert_eq!(result.error_code.as_deref(), Some("invalid_arguments"));
            assert!(!result.effects_unknown);
            assert!(!root.path().join("invalid").exists());
        }
        for (policy_timeout, requested_timeout) in [(2000, 0.05), (50, 2.0)] {
            let mut policy = policy(root.path());
            policy.shell_timeout_ms = policy_timeout;
            let (_sender, cancel) = tokio::sync::watch::channel(false);
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                service.run(
                    policy,
                    "bash",
                    json!({"command":"sleep 30","timeout":requested_timeout}),
                    cancel,
                    &mut |_, _| {},
                ),
            )
            .await
            .unwrap();
            assert_eq!(result.error_code.as_deref(), Some("tool_timeout"));
            assert!(result.effects_unknown);
        }
        let (_sender, cancel) = tokio::sync::watch::channel(false);
        let result = service.run(policy(root.path()), "bash", json!({"command":"items=(one two); printf '%s' \"${items[1]}\"; printf 'error' >&2; exit 7"}), cancel, &mut |_, _| {}).await;
        assert_eq!(result.error_code.as_deref(), Some("process_exit_failed"));
        assert!(!result.effects_unknown);
        let output: Value = serde_json::from_str(&result.output).unwrap();
        assert_eq!(output["stdout"], "two");
        assert_eq!(output["stderr"], "error");
        assert_eq!(output["exit_code"], 7);
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn bash_filters_credentials_caps_noisy_output_and_kills_descendants_on_cancel() {
        let root = tempfile::tempdir().unwrap();
        let service = Arc::new(ToolService::new(root.path().join("artifacts")).unwrap());
        let (_sender, cancel) = tokio::sync::watch::channel(false);
        let result = service
            .run(
                policy(root.path()),
                "bash",
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
                "bash",
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
                    "bash",
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
            // SIGKILL delivery to the descendant can finish after the shell
            // has been reaped. Require bounded termination, not an immediate
            // process-state transition in the same scheduler tick.
            tokio::time::timeout(Duration::from_secs(1), async {
                loop {
                    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"));
                    if stat.is_err() || stat.unwrap().split_once(") ").unwrap().1.starts_with('Z') {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
        }
    }
}
