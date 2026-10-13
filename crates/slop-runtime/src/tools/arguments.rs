use super::*;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
// Execution owns strings when crossing into a blocking worker; validation uses
// these same schemas with &str borrowed from the supplied JSON arguments.
pub(super) struct BashArgs<T = String> {
    pub(super) command: T,
    pub(super) timeout: Option<f64>,
}
fn first_line() -> usize {
    1
}
fn read_limit() -> usize {
    MAX_READ_LINES
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs<T = String> {
    path: T,
    #[serde(default = "first_line")]
    offset: usize,
    #[serde(default = "read_limit")]
    limit: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs<T = String> {
    path: T,
    content: T,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SingleEditArgs<T = String> {
    path: T,
    old_text: T,
    new_text: T,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Replacement<T = String> {
    old_text: T,
    new_text: T,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MultipleEditArgs<T = String> {
    path: T,
    edits: Vec<Replacement<T>>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum EditArgs<T = String> {
    Single(SingleEditArgs<T>),
    Multiple(MultipleEditArgs<T>),
}

impl<T> EditArgs<T> {
    fn into_parts(self) -> (T, Vec<Replacement<T>>) {
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
pub(super) fn file_tool(
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
            let total_lines = text.split_terminator('\n').count();
            if args.offset > total_lines && !(total_lines == 0 && args.offset == 1) {
                return Err("invalid_offset");
            }
            // Account for JSON escaping and metadata before selecting complete
            // lines. The returned page must fit the workspace output budget so
            // its text and continuation offset are never cut in half later.
            let overhead = json!({"path":args.path,"offset":args.offset,"next_offset":total_lines,"total_lines":total_lines,"text":""})
                .to_string()
                .len()
                + 4;
            let encoded_limit = policy
                .max_output_bytes
                .checked_sub(overhead)
                .ok_or("tool_output_limit")?;
            let mut selected = String::new();
            let mut count = 0;
            let mut encoded_bytes = 0;
            for line in text
                .split_terminator('\n')
                .skip(args.offset - 1)
                .take(args.limit.min(MAX_READ_LINES))
            {
                let separator = usize::from(count > 0);
                let line_bytes = serde_json::to_string(line).unwrap().len() - 2;
                if selected.len() + separator + line.len() > MAX_READ_BYTES
                    || encoded_bytes + separator * 2 + line_bytes > encoded_limit
                {
                    if count == 0 {
                        return Err("read_line_limit");
                    }
                    break;
                }
                if count > 0 {
                    selected.push('\n');
                }
                selected.push_str(line);
                encoded_bytes += separator * 2 + line_bytes;
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
pub(super) fn replace_file(
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
            "Read a UTF-8 workspace-relative file up to 1 MiB. Offset is a 1-based line number (default 1); limit is a line count (default 2000). Returns complete JSON containing at most 2000 complete lines and 256 KiB of text, with next_offset to continue. A smaller workspace output budget can shorten the page. A single line that cannot fit fails with read_line_limit. Empty files have zero lines; a final newline does not add an empty line. Absolute paths, parent traversal, and escapes are rejected. Images are not supported.",
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
            "Execute a Bash command from the workspace root with no startup profiles, a filtered environment, and bounded stdout/stderr. Returns JSON with exit_code, stdout, and stderr. Large outputs use head/tail previews with truncation flags and full bounded artifacts. Optional timeout is in seconds and can shorten the workspace deadline. Linux uses /bin/bash; Windows requires bash.exe on PATH. Commands may use cd for subdirectories. Bash is not sandboxed and may cause external changes. Background descendants are terminated when Bash exits.",
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
        "read" => ReadArgs::<&str>::deserialize(arguments)
            .is_ok_and(|args| args.offset > 0 && args.limit > 0),
        "write" => WriteArgs::<&str>::deserialize(arguments)
            .is_ok_and(|args| args.content.len() <= MAX_FILE_BYTES),
        "edit" => EditArgs::<&str>::deserialize(arguments).is_ok_and(|args| {
            let (_, edits) = args.into_parts();
            !edits.is_empty() && edits.iter().all(|edit| !edit.old_text.is_empty())
        }),
        "bash" => BashArgs::<&str>::deserialize(arguments).is_ok_and(|args| {
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
