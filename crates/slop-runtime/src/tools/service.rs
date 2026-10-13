use super::*;

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
        let result = tokio::task::spawn_blocking(move || {
            // The worker owns admission until it really finishes, even if its
            // awaiting future is dropped while a file operation is running.
            let _permit = permit;
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
