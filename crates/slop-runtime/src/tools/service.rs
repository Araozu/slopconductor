use super::*;

// Dropping an execution future must target the wrapper's process group/job,
// including when shutdown aborts the task before normal cleanup completes.
struct SupervisedChild(Box<dyn process_wrap::tokio::ChildWrapper>);
impl Drop for SupervisedChild {
    fn drop(&mut self) {
        let _ = self.0.start_kill();
    }
}

// Pipe reads can split a UTF-8 character at any byte. Retain incomplete
// characters across chunks so streamed text agrees with the canonical output.
#[derive(Default)]
struct StreamText {
    pending: Vec<u8>,
}

impl StreamText {
    fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut text = String::new();
        let mut consumed = 0;
        loop {
            match std::str::from_utf8(&self.pending[consumed..]) {
                Ok(valid) => {
                    text.push_str(valid);
                    consumed = self.pending.len();
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    text.push_str(
                        std::str::from_utf8(&self.pending[consumed..consumed + valid]).unwrap(),
                    );
                    consumed += valid;
                    if let Some(invalid) = error.error_len() {
                        text.push('\u{fffd}');
                        consumed += invalid;
                    } else {
                        break;
                    }
                }
            }
        }
        self.pending.drain(..consumed);
        text
    }

    fn finish(&mut self) -> String {
        let text = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        text
    }
}

fn excerpt(text: &str, budget: usize) -> String {
    if text.len() <= budget {
        return text.to_owned();
    }
    let mut head = budget / 2;
    let mut tail = text.len() - (budget - head);
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    format!("{}\n… output omitted …\n{}", &text[..head], &text[tail..])
}

fn bash_preview(stdout: &str, stderr: &str, exit_code: Option<i32>) -> String {
    let preview = |budget: usize| {
        let stdout_budget = (budget / 2)
            .max(budget.saturating_sub(stderr.len()))
            .min(stdout.len());
        let stderr_budget = budget.saturating_sub(stdout_budget).min(stderr.len());
        json!({
            "exit_code":exit_code,
            "stdout":excerpt(stdout, stdout_budget),
            "stderr":excerpt(stderr, stderr_budget),
            "stdout_bytes":stdout.len(),
            "stderr_bytes":stderr.len(),
            "stdout_truncated":stdout.len()>stdout_budget,
            "stderr_truncated":stderr.len()>stderr_budget,
        })
        .to_string()
    };
    // JSON escaping may expand a byte sixfold. Search the encoded budget
    // instead of cutting serialized JSON or a Unicode character in half.
    let (mut low, mut high) = (0, PREVIEW_BYTES);
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if preview(middle).len() <= PREVIEW_BYTES {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    preview(low)
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
                if output.len() > output_limit {
                    return ToolOutcome::failed("tool_output_limit", false);
                }
                self.complete_output(output, None, "application/json", None, false)
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
        output: String,
        preview: Option<String>,
        media_type: &str,
        error_code: Option<&str>,
        effects_unknown: bool,
    ) -> ToolOutcome {
        let mut artifacts = Vec::new();
        if output.len() > PREVIEW_BYTES {
            match self.save_artifact(output.as_bytes(), media_type).await {
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
            output: preview.unwrap_or(output),
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
        let mut out_text = StreamText::default();
        let mut err_text = StreamText::default();
        let mut status = None;
        let mut failure = None;
        let cleanup = tokio::time::sleep(Duration::from_secs(2));
        tokio::pin!(cleanup);
        while out_open || err_open || status.is_none() {
            let (stream, read) = tokio::select! {
                read=stdout.read(&mut out_buffer),if out_open=>("stdout",read),
                read=stderr.read(&mut err_buffer),if err_open=>("stderr",read),
                // The outer process-group/Job Object wait includes descendants.
                // Observe the wrapped Bash leader so background processes cannot
                // postpone the cleanup that is responsible for terminating them.
                result=child.inner_mut().wait(),if status.is_none()=>{
                    match result {
                        Ok(result)=>status=Some(result),
                        Err(_)=>{failure=Some("process_wait_failed");break;},
                    }
                    // A background descendant may keep the pipes open after
                    // Bash exits. End its lifetime now, then drain buffered
                    // output under a separate bounded cleanup deadline.
                    let _=child.start_kill();
                    cleanup.as_mut().reset(tokio::time::Instant::now()+Duration::from_secs(2));
                    continue;
                },
                _=&mut cleanup,if status.is_some()=>{failure=Some("process_output_failed");break;},
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
                let text = if stream == "stdout" {
                    out_text.finish()
                } else {
                    err_text.finish()
                };
                if !text.is_empty() {
                    on_output(stream, &text);
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
            let text = if stream == "stdout" {
                out_text.push(chunk)
            } else {
                err_text.push(chunk)
            };
            if !text.is_empty() {
                on_output(stream, &text);
            }
            if chunk.len() != buffer.len() {
                failure = Some("tool_output_limit");
                break;
            }
        }
        for (stream, text) in [("stdout", out_text.finish()), ("stderr", err_text.finish())] {
            if !text.is_empty() {
                on_output(stream, &text);
            }
        }
        let mut effects_unknown = failure.is_some();
        if failure.is_some() {
            if child.start_kill().is_err() {
                effects_unknown = true;
            }
            if status.is_none() {
                status = tokio::time::timeout(Duration::from_secs(2), child.wait())
                    .await
                    .ok()
                    .and_then(std::result::Result::ok);
            }
        }
        // Even a successful leader may leave descendants with closed pipes.
        // Supervised shell invocations do not grant background-process lifetime.
        let _ = child.start_kill();
        if status.is_none() {
            failure = Some("process_wait_failed");
            effects_unknown = true;
        } else if status.is_some_and(|status| !status.success()) && failure.is_none() {
            failure = Some("process_exit_failed");
        }
        let stdout = String::from_utf8_lossy(&out);
        let stderr = String::from_utf8_lossy(&err);
        let exit_code = status.and_then(|status| status.code());
        let output = json!({"exit_code":exit_code,"stdout":stdout,"stderr":stderr}).to_string();
        let preview = if output.len() > PREVIEW_BYTES {
            Some(bash_preview(&stdout, &stderr, exit_code))
        } else {
            None
        };
        self.complete_output(
            output,
            preview,
            "application/json",
            failure,
            effects_unknown,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_unicode_and_invalid_bytes_match_canonical_lossy_text() {
        for bytes in ["a🦀b€c".as_bytes(), b"a\xf0\x9f\xa6\x80\xff\xc0b\xe2\x82"] {
            for chunk_size in 1..=5 {
                let mut stream = StreamText::default();
                let mut text = String::new();
                for chunk in bytes.chunks(chunk_size) {
                    text.push_str(&stream.push(chunk));
                    assert!(stream.pending.len() <= 3);
                }
                text.push_str(&stream.finish());
                assert_eq!(text, String::from_utf8_lossy(bytes));
            }
        }
    }

    #[test]
    fn previews_bound_json_escaping_and_preserve_unicode_boundaries() {
        for stdout in ["\u{0001}".repeat(40000), "🦀".repeat(10000)] {
            let preview = bash_preview(&stdout, "failure €", Some(23));
            assert!(preview.len() <= PREVIEW_BYTES);
            let value: Value = serde_json::from_str(&preview).unwrap();
            assert_eq!(value["exit_code"], 23);
            assert_eq!(value["stderr"], "failure €");
            assert_eq!(value["stdout_truncated"], true);
            assert!(!value["stdout"].as_str().unwrap().contains('\u{fffd}'));
        }
    }
}
