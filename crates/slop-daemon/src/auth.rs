use std::{
    fmt,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::Path,
};

use subtle::ConstantTimeEq;
use thiserror::Error;

#[cfg(windows)]
use std::path::PathBuf;

const TOKEN_BYTES: usize = 32;

#[derive(Error)]
pub enum AuthError {
    #[error("credential storage is not private: {0}")]
    Insecure(String),
    #[error("credential file I/O failed: {0}")]
    Io(#[from] io::Error),
}

impl fmt::Debug for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Insecure(message) => f.debug_tuple("Insecure").field(message).finish(),
            Self::Io(error) => f.debug_tuple("Io").field(error).finish(),
        }
    }
}

/// The local bearer credential. Its debug representation never includes the token.
pub struct LocalToken(String);

impl fmt::Debug for LocalToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LocalToken([REDACTED])")
    }
}

impl LocalToken {
    pub fn from_data_dir(data_dir: &Path) -> Result<Self, AuthError> {
        let credentials_dir = data_dir.join("credentials");
        create_private_dir(&credentials_dir)?;
        let path = credentials_dir.join("local-api-token");
        if path.exists() || fs::symlink_metadata(&path).is_ok() {
            return read_existing(&path);
        }
        publish_new(&path)
    }

    pub fn matches(&self, candidate: &[u8]) -> bool {
        self.0.as_bytes().ct_eq(candidate).into()
    }
}

pub fn bearer_token(value: &str) -> Option<&[u8]> {
    let candidate = value.strip_prefix("Bearer ")?;
    if candidate.is_empty() || candidate.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return None;
    }
    Some(candidate.as_bytes())
}

fn create_private_dir(path: &Path) -> Result<(), AuthError> {
    #[cfg(unix)]
    let create_result = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700).create(path)
    };
    #[cfg(not(unix))]
    let create_result = fs::create_dir(path);
    match create_result {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(AuthError::Insecure(
            "credentials path must be a real directory".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        if metadata.uid() != rustix::process::geteuid().as_raw() {
            return Err(AuthError::Insecure(
                "credentials directory must be owned by the current user".into(),
            ));
        }
        if metadata.permissions().mode() & 0o777 != 0o700 {
            return Err(AuthError::Insecure(
                "credentials directory must have mode 0700".into(),
            ));
        }
    }
    #[cfg(windows)]
    if !is_under_local_app_data(path) {
        return Err(AuthError::Insecure("Windows credentials require a location beneath LOCALAPPDATA so inherited per-user ACLs apply".into()));
    }
    Ok(())
}

#[cfg(windows)]
fn is_under_local_app_data(path: &Path) -> bool {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .and_then(|base| {
            let base = fs::canonicalize(base).ok()?;
            let canonical_path = fs::canonicalize(path).ok()?;
            Some(canonical_path.starts_with(base))
        })
        .unwrap_or(false)
}

fn read_existing(path: &Path) -> Result<LocalToken, AuthError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AuthError::Insecure(
            "token must be a regular, non-symlink file".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(AuthError::Insecure("token file must have mode 0600".into()));
        }
        if metadata.uid() != rustix::process::geteuid().as_raw() {
            return Err(AuthError::Insecure(
                "token file must be owned by the current user".into(),
            ));
        }
    }
    let file = OpenOptions::new().read(true).open(path)?;
    let opened = file.metadata()?;
    if !opened.is_file() || !same_file(&metadata, &opened) {
        return Err(AuthError::Insecure(
            "token file changed while opening".into(),
        ));
    }
    validate_windows_acl_location(path)?;
    parse_token(file)
}

#[cfg(unix)]
fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_file(_left: &fs::Metadata, _right: &fs::Metadata) -> bool {
    true
}

#[cfg(windows)]
fn validate_windows_acl_location(path: &Path) -> Result<(), AuthError> {
    if is_under_local_app_data(path) {
        Ok(())
    } else {
        Err(AuthError::Insecure("Windows credentials require a location beneath LOCALAPPDATA so inherited per-user ACLs apply".into()))
    }
}

#[cfg(not(windows))]
fn validate_windows_acl_location(_path: &Path) -> Result<(), AuthError> {
    Ok(())
}

fn parse_token(file: File) -> Result<LocalToken, AuthError> {
    let mut content = Vec::new();
    file.take((TOKEN_BYTES * 2 + 2) as u64)
        .read_to_end(&mut content)?;
    if content.len() != TOKEN_BYTES * 2 + 1
        || content.last() != Some(&b'\n')
        || !content[..TOKEN_BYTES * 2].iter().all(u8::is_ascii_hexdigit)
    {
        return Err(AuthError::Insecure(
            "token file has an invalid format".into(),
        ));
    }
    let token = String::from_utf8(content[..TOKEN_BYTES * 2].to_vec())
        .map_err(|_| AuthError::Insecure("token file has an invalid format".into()))?;
    Ok(LocalToken(token))
}

fn publish_new(path: &Path) -> Result<LocalToken, AuthError> {
    let mut bytes = [0_u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|error| io::Error::other(error.to_string()))?;
    let token = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let mut temporary =
        tempfile::NamedTempFile::new_in(path.parent().unwrap_or_else(|| Path::new(".")))?;
    // tempfile creates owner-private files on Unix and inherits the parent
    // directory ACL on Windows.
    let file = temporary.as_file_mut();
    file.write_all(token.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    #[cfg(unix)]
    {
        fs::hard_link(temporary.path(), path)?;
        temporary.close()?;
        File::open(path.parent().unwrap_or_else(|| Path::new(".")))?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        temporary
            .persist_noclobber(path)
            .map_err(|error| error.error)?;
    }
    Ok(LocalToken(token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_persistent_private_and_redacted() {
        let temp = tempfile::tempdir().unwrap();
        let first = LocalToken::from_data_dir(temp.path()).unwrap();
        let debug = format!("{first:?}");
        assert!(debug.contains("REDACTED"));
        assert!(!debug.contains(&first.0));
        let second = LocalToken::from_data_dir(temp.path()).unwrap();
        assert!(second.matches(first.0.as_bytes()));
        assert!(!second.matches(b"wrong"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = fs::metadata(temp.path().join("credentials/local-api-token")).unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn bearer_header_requires_one_exact_token() {
        assert_eq!(bearer_token("Bearer abc"), Some(b"abc".as_slice()));
        assert_eq!(bearer_token("Bearer "), None);
        assert_eq!(bearer_token("bearer abc"), None);
        assert_eq!(bearer_token("Bearer abc extra"), None);
        assert_eq!(bearer_token("Bearer abc\n"), None);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_token_is_rejected() {
        use std::os::unix::fs::PermissionsExt;
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let credentials = temp.path().join("credentials");
        fs::create_dir(&credentials).unwrap();
        fs::set_permissions(&credentials, fs::Permissions::from_mode(0o700)).unwrap();
        let target = temp.path().join("target");
        fs::write(&target, "secret\n").unwrap();
        symlink(target, credentials.join("local-api-token")).unwrap();
        assert!(LocalToken::from_data_dir(temp.path()).is_err());
    }
}
