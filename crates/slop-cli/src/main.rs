use std::{error::Error, process::ExitCode};

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

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check daemon connectivity, API compatibility, and capabilities.
    Status,
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
    let client = DaemonClient::new(&args.daemon)?;

    match args.command {
        Command::Status => {
            let health = client.health().await?;
            if args.json {
                println!("{}", serde_json::to_string(&health)?);
            } else {
                println!("{} {}", health.service, health.version);
                println!("API version: {}", health.api_version);
                println!("Capabilities: {}", health.capabilities.join(", "));
            }
        }
    }

    Ok(())
}
