use super::Context;
use crate::{Result, args::PeerCommand};
use hive_protocol::peers::{CreatePeerGrantRequest, PeerScope, RegisterPeerRequest};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Path,
};

fn read_secret(path: &Path) -> Result<String> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("peer credential must be a regular non-symlink file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err("peer credential file must have mode 0600".into());
        }
    }
    if !(64..=66).contains(&metadata.len()) {
        return Err("peer credential file has an invalid length".into());
    }
    let mut value = String::new();
    fs::File::open(path)?.take(67).read_to_string(&mut value)?;
    let value = value.trim_end_matches(['\r', '\n']);
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("peer credential must contain 64 lowercase hexadecimal digits".into());
    }
    Ok(value.to_owned())
}
fn grant_secret(path: &Path) -> Result<String> {
    if fs::symlink_metadata(path).is_ok() {
        return read_secret(path);
    }
    let token = format!(
        "{}{}",
        hive_client::new_command_id()?,
        hive_client::new_command_id()?
    );
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        let base = std::env::var_os("LOCALAPPDATA")
            .ok_or("LOCALAPPDATA is required for private peer credentials")?;
        let parent = fs::canonicalize(path.parent().ok_or("credential parent is required")?)?;
        if !parent.starts_with(fs::canonicalize(base)?) {
            return Err("Windows peer credentials require a path beneath LOCALAPPDATA".into());
        }
    }
    let mut file = options.open(path)?;
    writeln!(file, "{token}")?;
    file.sync_all()?;
    #[cfg(unix)]
    fs::File::open(
        path.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?
    .sync_all()?;
    Ok(token)
}
pub(super) async fn run(command: PeerCommand, context: &Context) -> Result<()> {
    if context.connection.peer.is_some() {
        return Err("peer administration targets the local daemon; omit --peer".into());
    }
    let client = context.client()?;
    match command {
        PeerCommand::List => {
            let value = client.peers().await?;
            context.output.value(&value, || {
                for p in &value {
                    println!(
                        "{}\t{}\t{}\tremoved={}",
                        p.id, p.owner_node_id, p.origin, p.removed
                    );
                }
            })?;
        }
        PeerCommand::Grants => {
            let value = client.peer_grants().await?;
            context.output.value(&value, || {
                for g in &value {
                    println!(
                        "{}\t{}\t{:?}\trevoked={}",
                        g.id, g.label, g.scope, g.revoked
                    );
                }
            })?;
        }
        PeerCommand::Add {
            id,
            origin,
            owner_node_id,
            credential_file,
        } => {
            let request = RegisterPeerRequest {
                origin,
                owner_node_id,
                token: read_secret(&credential_file)?,
            };
            let value = client.register_peer(&id, &request).await?;
            context.output.value(&value, || {
                println!("registered {} ({})", value.id, value.owner_node_id)
            })?;
        }
        PeerCommand::Probe { id } => {
            let value = client.probe_peer(&id).await?;
            context.output.value(&value, || {
                println!(
                    "{}: {} ({}, {})",
                    id, value.node.name, value.node.node_id, value.node.os
                )
            })?;
        }
        PeerCommand::Remove { id } => {
            let value = client.remove_peer(&id).await?;
            context
                .output
                .value(&value, || println!("removed {}", value.id))?;
        }
        PeerCommand::Grant {
            label,
            scope,
            token_output,
            command_id,
        } => {
            let request = CreatePeerGrantRequest {
                label,
                scope: if scope == "control" {
                    PeerScope::Control
                } else {
                    PeerScope::Read
                },
                token: grant_secret(&token_output)?,
            };
            let value = client
                .create_peer_grant(&command_id, &request)
                .await
                .map_err(|e| crate::mutation::mutation_error(e, &command_id))?;
            context.output.value(&value, || {
                println!(
                    "grant {} on owner {} ({:?}); credential saved to {}",
                    value.id,
                    value.owner_node_id,
                    value.scope,
                    token_output.display()
                )
            })?;
        }
        PeerCommand::Revoke { id } => {
            let value = client.revoke_peer_grant(&id).await?;
            context
                .output
                .value(&value, || println!("revoked {}", value.id))?;
        }
    }
    Ok(())
}
