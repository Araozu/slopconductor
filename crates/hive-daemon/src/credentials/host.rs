use super::*;

/// OAuth accepts a UUID URI, not Hive's unrelated opaque node identifier.
pub(super) fn load_host_id(directory: &Path) -> Result<String, CredentialError> {
    let path = directory.join("codex-host-id");
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {
            let mut value = String::new();
            open_private_file(&path)
                .map_err(|_| CredentialError::Storage)?
                .take(128)
                .read_to_string(&mut value)
                .map_err(|_| CredentialError::Storage)?;
            let uuid = value
                .strip_prefix("urn:uuid:")
                .ok_or(CredentialError::Storage)?;
            if uuid.len() != 36
                || !uuid.bytes().enumerate().all(|(i, b)| {
                    if [8, 13, 18, 23].contains(&i) {
                        b == b'-'
                    } else {
                        b.is_ascii_hexdigit()
                    }
                })
                || uuid.as_bytes()[14] != b'4'
                || !matches!(uuid.as_bytes()[19], b'8' | b'9' | b'a' | b'b')
            {
                return Err(CredentialError::Storage);
            }
            Ok(value)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut bytes = [0_u8; 16];
            getrandom::fill(&mut bytes).map_err(|_| CredentialError::Storage)?;
            bytes[6] = (bytes[6] & 0x0f) | 0x40;
            bytes[8] = (bytes[8] & 0x3f) | 0x80;
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            let value = format!(
                "urn:uuid:{}-{}-{}-{}-{}",
                &hex[..8],
                &hex[8..12],
                &hex[12..16],
                &hex[16..20],
                &hex[20..]
            );
            write_private_file(&path, value.as_bytes()).map_err(|_| CredentialError::Storage)?;
            Ok(value)
        }
        Err(_) => Err(CredentialError::Storage),
    }
}
