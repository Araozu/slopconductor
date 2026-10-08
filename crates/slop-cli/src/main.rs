use std::{error::Error, path::PathBuf, process::ExitCode};

use clap::{Parser, Subcommand};
use slop_client::DaemonClient;
use slop_protocol::DEFAULT_DAEMON_URL;

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Debug, Parser)]
#[command(name = "slop", version, about = "Slop Conductor command-line client")]
struct Args {
    /// Daemon origin to connect to.
    #[arg(long, global = true, env = "SLOP_DAEMON_URL", default_value = DEFAULT_DAEMON_URL)]
    daemon: String,

    /// Emit machine-readable JSON on standard output.
    #[arg(long, global = true)]
    json: bool,

    /// Read the local API bearer token from this file for authenticated commands.
    #[arg(long, global = true)]
    token_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check daemon connectivity, API compatibility, and capabilities.
    Status,
    /// Show this daemon's authenticated node identity.
    Node,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> ExitCode {
    match run(Args::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("slop: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<()> {
    match args.command {
        Command::Status => {
            let client = DaemonClient::new(&args.daemon)?;
            let health = client.health().await?;
            if args.json {
                println!("{}", serde_json::to_string(&health)?);
            } else {
                println!("{} {}", health.service, health.version);
                println!("API version: {}", health.api_version);
                println!("Capabilities: {}", health.capabilities.join(", "));
            }
        }
        Command::Node => {
            let token_file = match args.token_file {
                Some(path) => path,
                None if is_loopback_endpoint(&args.daemon) => std::env::var_os("SLOP_TOKEN_FILE")
                    .map(PathBuf::from)
                    .ok_or("node requires --token-file or SLOP_TOKEN_FILE")?,
                None => return Err("remote node queries require an explicit --token-file".into()),
            };
            let client = DaemonClient::new_with_token_file(&args.daemon, &token_file)?;
            let node = client.node().await?;
            if args.json {
                println!("{}", serde_json::to_string(&node)?);
            } else {
                println!("{} ({})", node.name, node.node_id);
                println!("OS: {}", node.os);
            }
        }
    }

    Ok(())
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
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}
