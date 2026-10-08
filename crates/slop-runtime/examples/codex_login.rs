//! Short-lived login helper; it does not run an agent or make inference calls.

use std::{path::PathBuf, process::ExitCode};

use slop_runtime::providers::chatgpt_auth::{ChatGptConnection, ChatGptLogin};

#[tokio::main]
async fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(path), Some(host_id), None) = (args.next(), args.next(), args.next()) else {
        eprintln!("usage: codex_login <credential-file> <stable-host-id>");
        return ExitCode::FAILURE;
    };
    let path = PathBuf::from(path);
    let result = async {
        let login = if path.exists() {
            ChatGptConnection::load(&path).await?.begin_login().await?
        } else {
            ChatGptLogin::begin(&host_id).await?
        };
        println!("Continue with ChatGPT: {}", login.authorization_url());
        login.finish(&path).await?;
        Ok::<_, slop_runtime::providers::ProviderError>(())
    }
    .await;
    match result {
        Ok(()) => {
            println!("ChatGPT connection saved.");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
