use super::chat::error;
use axum::{
    Json,
    extract::Query,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use slop_protocol::filesystem::{CreateDirectoryRequest, DirectoryEntry, DirectoryResponse};
use std::path::{Component, Path, PathBuf};

#[derive(Deserialize)]
pub struct DirectoryQuery {
    path: Option<String>,
    after: Option<String>,
    limit: Option<usize>,
}

fn canonical_directory(path: &Path) -> std::io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Use an absolute directory path.",
        ));
    }
    let path = std::fs::canonicalize(path)?;
    if !path.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "The path is not a directory.",
        ));
    }
    Ok(path)
}

fn listing(query: DirectoryQuery) -> std::io::Result<DirectoryResponse> {
    let limit = query.limit.unwrap_or(200);
    if !(1..=200).contains(&limit) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Limit must be between 1 and 200.",
        ));
    }
    let start = query.path.map(PathBuf::from).unwrap_or_else(|| {
        std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(std::path::MAIN_SEPARATOR.to_string()))
    });
    let path = canonical_directory(&start)?;
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&path)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if query.after.as_ref().is_some_and(|after| name <= *after) {
            continue;
        }
        let ty = entry.file_type()?;
        entries.push(DirectoryEntry {
            name,
            path: entry.path().to_string_lossy().into_owned(),
            kind: if ty.is_dir() {
                "directory"
            } else if ty.is_symlink() {
                "symlink"
            } else {
                "file"
            }
            .into(),
        });
        // Keep memory bounded even for very large directories.
        if entries.len() > limit * 2 {
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            entries.truncate(limit + 1);
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    let more = entries.len() > limit;
    entries.truncate(limit);
    Ok(DirectoryResponse {
        parent: path.parent().map(|p| p.to_string_lossy().into_owned()),
        path: path.to_string_lossy().into_owned(),
        next_after: if more {
            entries.last().map(|e| e.name.clone())
        } else {
            None
        },
        entries,
    })
}

fn io_response(result: std::io::Result<DirectoryResponse>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(err) => {
            let status = match err.kind() {
                std::io::ErrorKind::NotFound => StatusCode::NOT_FOUND,
                std::io::ErrorKind::PermissionDenied => StatusCode::FORBIDDEN,
                std::io::ErrorKind::AlreadyExists => StatusCode::CONFLICT,
                std::io::ErrorKind::InvalidInput => StatusCode::BAD_REQUEST,
                _ => StatusCode::INTERNAL_SERVER_ERROR,
            };
            error(status, "filesystem_error", &err.to_string())
        }
    }
}

#[utoipa::path(
    get,
    path = "/v1/filesystem",
    tag = "projects",
    params(
        ("path" = Option<String>, Query, description = "Absolute directory path; defaults to the home directory"),
        ("after" = Option<String>, Query, description = "Entry name after which to page"),
        ("limit" = Option<usize>, Query, description = "Page limit, 1-200")
    ),
    responses(
        (status = 200, description = "Directory listing", body = DirectoryResponse),
        (status = 400, description = "Not a directory or invalid limit", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 403, description = "Permission denied", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Path not found", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub async fn browse(Query(query): Query<DirectoryQuery>) -> Response {
    match tokio::task::spawn_blocking(move || listing(query)).await {
        Ok(result) => io_response(result),
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "filesystem_error",
            "Filesystem worker failed.",
        ),
    }
}

#[utoipa::path(
    post,
    path = "/v1/filesystem/directories",
    tag = "projects",
    request_body = CreateDirectoryRequest,
    responses(
        (status = 200, description = "Created directory listing", body = DirectoryResponse),
        (status = 400, description = "Invalid folder name", body = slop_protocol::ErrorResponse),
        (status = 401, description = "Missing or invalid bearer token", body = slop_protocol::ErrorResponse),
        (status = 404, description = "Parent not found", body = slop_protocol::ErrorResponse),
        (status = 409, description = "Path already exists", body = slop_protocol::ErrorResponse)
    ),
    security(("bearer_auth" = []))
)]
pub async fn create(Json(request): Json<CreateDirectoryRequest>) -> Response {
    match tokio::task::spawn_blocking(move || {
        let mut components = Path::new(&request.name).components();
        if request.name.is_empty()
            || request.name.contains(['/', '\\'])
            || !matches!(components.next(), Some(Component::Normal(_)))
            || components.next().is_some()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "Use a single folder name.",
            ));
        }
        let parent = canonical_directory(Path::new(&request.parent))?;
        let path = parent.join(request.name);
        // Never adopt an existing path or replay a filesystem mutation.
        std::fs::create_dir(&path)?;
        listing(DirectoryQuery {
            path: Some(path.to_string_lossy().into_owned()),
            after: None,
            limit: None,
        })
    })
    .await
    {
        Ok(result) => io_response(result),
        Err(_) => error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "filesystem_error",
            "Filesystem worker failed.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn navigation_and_pagination() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::create_dir(root.join("a")).unwrap();
        std::fs::write(root.join("b"), "file").unwrap();
        let page = listing(DirectoryQuery {
            path: Some(root.to_string_lossy().into()),
            after: None,
            limit: Some(1),
        })
        .unwrap();
        assert_eq!(page.entries[0].kind, "directory");
        assert_eq!(page.next_after.as_deref(), Some("a"));
        let page = listing(DirectoryQuery {
            path: Some(root.to_string_lossy().into()),
            after: page.next_after,
            limit: Some(1),
        })
        .unwrap();
        assert_eq!(page.entries[0].kind, "file");
        assert!(page.next_after.is_none());
        assert!(canonical_directory(Path::new("relative")).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
