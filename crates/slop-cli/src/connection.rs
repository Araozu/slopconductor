use std::path::PathBuf;

use slop_client::DaemonClient;

use crate::Result;

pub struct ConnectionOptions {
    pub daemon: String,
    pub token_file: Option<PathBuf>,
}

pub fn authenticated_client(args: &ConnectionOptions) -> Result<DaemonClient> {
    let local = is_loopback_endpoint(&args.daemon);
    let token_file = match args.token_file.as_ref() {
        Some(path) => path.clone(),
        None if local => default_token_file()?.ok_or(
            "node requires --token-file, SLOP_TOKEN_FILE, or the default local token file",
        )?,
        None => return Err("remote authentication requires an explicit --token-file".into()),
    };
    if local && !token_file.is_file() {
        return Err(format!(
            "node requires --token-file or a configured local token file (not found: {})",
            token_file.display()
        )
        .into());
    }
    Ok(DaemonClient::new_with_token_file(
        &args.daemon,
        &token_file,
    )?)
}

fn default_token_file() -> Result<Option<PathBuf>> {
    if let Some(value) = std::env::var_os("SLOP_TOKEN_FILE") {
        return Ok(Some(value.into()));
    }
    #[cfg(windows)]
    {
        Ok(std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|root| {
                root.join("slopconductor")
                    .join("credentials")
                    .join("local-api-token")
            }))
    }
    #[cfg(not(windows))]
    {
        if let Some(root) = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
        {
            return Ok(Some(root.join("slopconductor/credentials/local-api-token")));
        }
        Ok(std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(".local/share/slopconductor/credentials/local-api-token")))
    }
}

fn is_loopback_endpoint(endpoint: &str) -> bool {
    let authority = endpoint
        .split_once("://")
        .map(|(_, rest)| rest.split('/').next().unwrap_or_default())
        .unwrap_or_default();
    let host = if authority.starts_with('[') {
        authority
            .split_once(']')
            .map(|(host, _)| &host[1..])
            .unwrap_or_default()
    } else {
        authority.split(':').next().unwrap_or_default()
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}
