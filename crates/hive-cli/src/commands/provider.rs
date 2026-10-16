use std::{
    io::{self, IsTerminal, Read},
    path::Path,
    time::Duration,
};

use hive_protocol::providers::{
    LoginCallbackRequest, MAX_API_KEY_BYTES, ProviderStatus, SelectAuthRequest, SetApiKeyRequest,
    StartLoginRequest,
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
        ProviderCommand::Login {
            command_id,
            account,
            headless,
            ..
        } => {
            let command_id = choose_command_id(command_id)?;
            let login = client
                .start_codex_login(&StartLoginRequest {
                    command_id: command_id.clone(),
                    account_id: account,
                    headless,
                })
                .await
                .map_err(|error| mutation_error(error, &command_id))?;
            context.output.value(&login, || {
                println!("Login ID: {}", login.login_id);
                println!();
                println!("Continue with ChatGPT:");
                println!("{}", login.authorization_url);
                println!();
                if login.headless {
                    println!("Open the link on your browser machine. After authorization, copy the full http://127.0.0.1:.../auth/callback?... URL from the address bar (the page may fail to connect).");
                }
            })?;
            if headless {
                eprintln!("Paste the final callback URL here and press Enter:");
            }
            let callback = read_callback_url();
            tokio::pin!(callback);
            let mut callback_submitted = !headless;
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
                    value = &mut callback, if !callback_submitted => {
                        client.submit_codex_login_callback(&login.login_id, &LoginCallbackRequest {
                            callback_url: value?,
                        }).await?;
                        callback_submitted = true;
                    },
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
        ProviderCommand::Discover { provider } => {
            let catalog = client.provider_catalog(&provider).await?;
            context.output.value(&catalog, || {
                println!(
                    "{} catalog ({}, observed {})",
                    catalog.provider, catalog.auth_mode, catalog.observed_at_unix_ms
                );
                for model in &catalog.models {
                    println!(
                        "{}{}",
                        model.id,
                        if model.ready { "" } else { " (unavailable)" }
                    );
                }
            })
        }
        ProviderCommand::Accounts => {
            let accounts = client.codex_accounts().await?;
            context.output.value(&accounts, || {
                for account in &accounts {
                    println!(
                        "{}: {}{}",
                        account.id,
                        if account.authenticated {
                            "signed in"
                        } else {
                            "signed out"
                        },
                        if account.active { " (active)" } else { "" }
                    );
                }
            })
        }
        ProviderCommand::Select { auth_mode, account } => {
            let status = client
                .select_codex_auth(&SelectAuthRequest {
                    auth_mode,
                    account_id: account,
                })
                .await?;
            context.output.value(&status, || print_status(&status))
        }
        ProviderCommand::Logout { account } => {
            let status = client.sign_out_codex_account(&account).await?;
            context.output.value(&status, || {
                println!("ChatGPT account {} signed out", status.account_id);
                if !status.remote_revocation_confirmed { println!("Remote revocation was not confirmed. Disconnect the app in ChatGPT Settings to revoke access."); }
            })
        }
        ProviderCommand::RemoveKey { provider } => {
            let status = client.remove_provider_api_key(&provider).await?;
            context.output.value(&status, || print_status(&status))
        }
    }
}

async fn read_callback_url() -> Result<String> {
    // Tokio stdin uses a blocking-pool read that can prevent runtime shutdown
    // on Ctrl-C or login timeout. A detached input thread lets the CLI exit.
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("login-callback-input".into())
        .spawn(move || {
            use std::io::BufRead;
            let mut reader = io::BufReader::new(io::stdin().lock().take(16 * 1024 + 1));
            let mut value = String::new();
            let result = reader.read_line(&mut value).map(|_| value);
            let _ = sender.send(result);
        })?;
    let value = receiver
        .await
        .map_err(|_| "callback input reader stopped")??;
    if value.len() > 16 * 1024 || value.trim().is_empty() {
        return Err(
            "supply one callback URL on stdin, at most 16 KiB; resume with the same --command-id"
                .into(),
        );
    }
    Ok(value.trim().to_owned())
}

fn print_status(status: &ProviderStatus) {
    let mode = if status.provider == "codex" {
        format!(
            " (active mode: {})",
            status.active_auth_mode.as_deref().unwrap_or("none")
        )
    } else {
        String::new()
    };
    println!(
        "{}: API key {}, ChatGPT {}, daemon chat {}{}",
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
        },
        mode
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
