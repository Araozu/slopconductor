use std::{error::Error, net::SocketAddr, process::ExitCode};

use axum::{Json, Router, routing::get};
use clap::Parser;
use slop_protocol::{API_VERSION, HEALTH_PATH, HealthResponse, SERVICE_NAME};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

#[derive(Debug, Parser)]
#[command(name = "slopd", version, about = "Slop Conductor machine daemon")]
struct Args {
    /// Local listen address. This bootstrap only permits loopback listeners.
    #[arg(long, env = "SLOP_LISTEN", default_value = "127.0.0.1:7331")]
    listen: SocketAddr,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Args::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("slopd: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn run(args: Args) -> Result<()> {
    if !args.listen.ip().is_loopback() {
        return Err("the bootstrap daemon only supports loopback listeners".into());
    }

    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    let address = listener.local_addr()?;
    eprintln!("Listening on http://{address}");

    let app = Router::new().route(HEALTH_PATH, get(health));
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            if let Err(error) = tokio::signal::ctrl_c().await {
                eprintln!("slopd: shutdown signal handler failed: {error}");
            }
        })
        .await?;

    Ok(())
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        service: SERVICE_NAME.to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        api_version: API_VERSION,
        capabilities: vec!["health".to_owned()],
    })
}
