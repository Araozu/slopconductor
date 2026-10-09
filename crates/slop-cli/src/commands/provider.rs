use std::{
    io::{self, IsTerminal, Read},
    path::Path,
    time::Duration,
};

use slop_protocol::providers::{
    MAX_API_KEY_BYTES, ProviderStatus, SetApiKeyRequest, StartLoginRequest,
};

use super::Context;
use crate::{
    Result,
    args::ProviderCommand,
    mutation::{choose_command_id, mutation_error},
};

pub(super) async fn run(command: ProviderCommand, context: &Context) -> Result<()> {
    let client = context.client()?;
    match command {
        ProviderCommand::Status => {
            let statuses = client.providers().await?;
            context.output.value(&statuses, || {
                for status in &statuses {
                    print_status(status);
                }
            })
        }
        ProviderCommand::SetKey { provider, key_file } => {
            let key = read_key(key_file.as_deref()).await?;
            let status = client
                .set_provider_api_key(&provider, &SetApiKeyRequest { api_key: key })
                .await?;
            context.output.value(&status, || print_status(&status))
        }
        ProviderCommand::Login { command_id, .. } => {
            let command_id = choose_command_id(command_id)?;
            let login = client
                .start_codex_login(&StartLoginRequest {
                    command_id: command_id.clone(),
                })
                .await
                .map_err(|error| mutation_error(error, &command_id))?;
            context.output.value(&login, || {
                println!("Continue with ChatGPT: {}", login.authorization_url);
                println!("Login ID: {}", login.login_id);
            })?;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(360);
            loop {
                let status = client.codex_login_status(&login.login_id).await?;
                if status.status != "pending" {
                    context
                        .output
                        .value(&status, || println!("ChatGPT login {}", status.status))?;
                    return if status.status == "succeeded" {
                        Ok(())
                    } else {
                        Err("ChatGPT login failed; start a new login attempt".into())
                    };
                }
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => return Ok(()),
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {},
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(
                        "login is still pending; inspect or retry with the same --command-id"
                            .into(),
                    );
                }
            }
        }
        ProviderCommand::LoginStatus { login_id } => {
            let status = client.codex_login_status(&login_id).await?;
            context
                .output
                .value(&status, || println!("ChatGPT login {}", status.status))
        }
    }
}

fn print_status(status: &ProviderStatus) {
    println!(
        "{}: API key {}, ChatGPT {}, daemon chat {}",
        status.provider,
        if status.api_key_configured {
            "saved"
        } else {
            "unset"
        },
        if status.chatgpt_configured {
            "saved"
        } else {
            "unset"
        },
        if status.execution_supported {
            "supported"
        } else {
            "planned"
        }
    );
}

async fn read_key(path: Option<&Path>) -> Result<String> {
    let mut bytes = Vec::new();
    if let Some(path) = path {
        std::fs::File::open(path)?
            .take((MAX_API_KEY_BYTES + 3) as u64)
            .read_to_end(&mut bytes)?;
    } else {
        if io::stdin().is_terminal() {
            return Err("pipe the API key on stdin or use --key-file PATH".into());
        }
        use tokio::io::AsyncReadExt;
        tokio::io::stdin()
            .take((MAX_API_KEY_BYTES + 3) as u64)
            .read_to_end(&mut bytes)
            .await?;
    }
    let value = String::from_utf8(bytes).map_err(|_| "API key input must be UTF-8")?;
    if value.len() > MAX_API_KEY_BYTES + 2 {
        return Err("API key input exceeds the 16 KiB limit".into());
    }
    let key = value.trim();
    if key.is_empty()
        || key.len() > MAX_API_KEY_BYTES
        || !key.bytes().all(|byte| (33..=126).contains(&byte))
    {
        return Err(
            "API key must be nonempty printable ASCII without whitespace, at most 16 KiB".into(),
        );
    }
    Ok(key.to_owned())
}
