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
    let output = file_tool(&policy, "read", json!({"path":"file","offset":2,"limit":1})).unwrap();
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
