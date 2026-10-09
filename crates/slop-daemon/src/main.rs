mod api;
mod auth;
mod config;
mod credentials;
mod storage;

use std::{
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode, header::AUTHORIZATION},
    response::IntoResponse,
    routing::get,
};
use clap::Parser;
use config::StartupArgs;
use slop_protocol::{
    API_VERSION, ErrorResponse, HEALTH_PATH, HealthResponse, NODE_PATH, SERVICE_NAME,
};
use tokio::{task::JoinHandle, time::timeout};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, Parser)]
#[command(name = "slopd", version, about = "Slop Conductor machine daemon")]
struct Args {
    #[command(flatten)]
    startup: StartupArgs,
    /// Override the configured display name for this node.
    #[arg(long, env = "SLOP_NODE_NAME")]
    name: Option<String>,
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
    let config = config::load_with_environment_and_name(
        args.startup,
        args.name,
        &config::ConfigEnvironment::current(),
    )?;

    // Signal subscriptions exist before any readiness message or bound socket.
    let signals = Signals::install()?;
    tokio::task::yield_now().await;
    let store = storage::Store::open(
        config.data_dir.clone(),
        config.node_name.clone(),
        config.database_queue_capacity,
        config.database_busy_timeout,
    )
    .await?;
    let token = auth::LocalToken::from_data_dir(&config.data_dir)?;
    let token = Arc::new(token);
    let store_client = store.client();
    let bootstrap = credentials::PROVIDERS
        .iter()
        .filter_map(|(provider, variable)| {
            std::env::var(variable)
                .ok()
                .map(|key| ((*provider).to_owned(), key))
        })
        .collect();
    let (credentials, _go_key) = credentials::ProviderCredentials::load_with_endpoints(
        &config.data_dir,
        store_client.node().await?.node_id,
        bootstrap,
        config.opencode_zen_base_url.clone(),
        config.codex_base_url.clone(),
    )
    .await?;
    // Bind first so a port/configuration failure cannot start queued inference
    // without a service that can acknowledge or observe it.
    let listener = tokio::net::TcpListener::bind(config.listen).await?;
    let address = listener.local_addr()?;
    let provider_clients = credentials
        .provider_clients(config.provider_base_url.as_deref())
        .await?;
    let tools = Arc::new(slop_runtime::tools::ToolService::new(
        config.data_dir.join("artifacts"),
    )?);
    let chat_runtime = slop_runtime::chat::start_with_provider_clients(
        Arc::new(store_client.clone()),
        provider_clients,
        config.provider_base_url.clone(),
        config.execution_concurrency,
        Some(config.default_model.clone()),
        Some(config.max_output_tokens),
        tools,
    );
    let accepting = Arc::new(AtomicBool::new(true));
    let state = Arc::new(api::chat::AppState {
        store: store_client.clone(),
        token,
        runtime: chat_runtime.clone(),
        accepting: Arc::clone(&accepting),
        event_subscribers: Arc::new(tokio::sync::Semaphore::new(128)),
        credentials,
    });

    // Config validation, exclusive ownership, database initialization,
    // credential publication, runtime startup, and binding complete before
    // the listener begins serving requests.
    let app = Router::new()
        .route(HEALTH_PATH, get(health))
        .route(NODE_PATH, get(node))
        .merge(api::chat::router(Arc::clone(&state.token)))
        .merge(api::providers::router(Arc::clone(&state.token)))
        .with_state(state.clone());
    eprintln!("Listening on http://{address}");

    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();
    let mut server: JoinHandle<std::io::Result<()>> = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async {
                let _ = shutdown_rx.await;
            })
            .await
    });
    let mut failure = None;
    let server_ended = tokio::select! {
        result = &mut server => {
            match result {
                Ok(Ok(())) => failure = Some("HTTP server stopped before a shutdown signal".to_owned()),
                Ok(Err(_)) => failure = Some("HTTP server failed".to_owned()),
                Err(_) => failure = Some("HTTP server task failed".to_owned()),
            }
            true
        }
        signal_result = signals.wait() => {
            if signal_result.is_err() { failure = Some("shutdown signal handler failed".to_owned()); }
            false
        },
    };
    accepting.store(false, Ordering::Release);
    if !server_ended {
        let _ = shutdown_tx.send(());
    }

    let deadline = Instant::now() + config.shutdown_timeout;
    let mut complete = true;
    if !server_ended {
        match timeout(remaining(deadline), &mut server).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(_))) => {
                failure = Some("HTTP server failed during graceful shutdown".to_owned());
                complete = false;
            }
            Ok(Err(_)) => {
                failure = Some("HTTP server task failed during graceful shutdown".to_owned());
                complete = false;
            }
            Err(_) => {
                complete = false;
                server.abort();
                let _ = server.await;
            }
        }
    }

    // Keep directory ownership until secret writes, login, active turns, and
    // finally the DB worker have drained, even if the shared deadline expires.
    let shutdown_state = Arc::clone(&state);
    let shutdown = tokio::spawn(async move {
        shutdown_state.credentials.shutdown().await;
        shutdown_state.runtime.shutdown().await;
        store.shutdown().await
    });
    match timeout(remaining(deadline), shutdown).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(error))) => {
            eprintln!("slopd: database shutdown failed: {error}");
            complete = false;
        }
        Ok(Err(error)) => {
            eprintln!("slopd: database shutdown task failed: {error}");
            complete = false;
        }
        Err(_) => {
            eprintln!("slopd: shutdown deadline elapsed; daemon resources are still draining");
            complete = false;
        }
    }
    if !complete {
        return Err("shutdown did not complete within the configured deadline".into());
    }
    if let Some(failure) = failure {
        return Err(failure.into());
    }
    Ok(())
}

fn remaining(deadline: Instant) -> std::time::Duration {
    deadline.saturating_duration_since(Instant::now())
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        service: SERVICE_NAME.to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        api_version: API_VERSION,
        capabilities: vec![
            "health".to_owned(),
            "node".to_owned(),
            "sessions".to_owned(),
            "text-chat".to_owned(),
            "provider-credentials".to_owned(),
            "structured-messages".to_owned(),
            "tools".to_owned(),
            "per-turn-settings".to_owned(),
            "execution-steering".to_owned(),
            "turn-pause-resume".to_owned(),
        ],
    })
}

async fn node(
    State(state): State<Arc<api::chat::AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !authorized(&headers, &state.token) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                code: "unauthorized".into(),
                message: "A valid bearer token is required.".into(),
            }),
        )
            .into_response();
    }
    match state.store.node().await {
        Ok(node) => (StatusCode::OK, Json(node)).into_response(),
        Err(error) => {
            let code = if matches!(error, storage::StoreError::Busy) {
                "storage_busy"
            } else {
                "storage_unavailable"
            };
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(ErrorResponse {
                    code: code.into(),
                    message: "Node storage is temporarily unavailable.".into(),
                }),
            )
                .into_response()
        }
    }
}

fn authorized(headers: &HeaderMap, token: &auth::LocalToken) -> bool {
    let mut values = headers.get_all(AUTHORIZATION).iter();
    let Some(value) = values.next() else {
        return false;
    };
    if values.next().is_some() {
        return false;
    }
    let Ok(value) = value.to_str() else {
        return false;
    };
    let Some(candidate) = auth::bearer_token(value) else {
        return false;
    };
    token.matches(candidate)
}

struct Signals {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(not(unix))]
    ctrl_c: tokio::sync::oneshot::Receiver<std::io::Result<()>>,
}

impl Signals {
    fn install() -> Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Ok(Self {
                interrupt: signal(SignalKind::interrupt())?,
                terminate: signal(SignalKind::terminate())?,
            })
        }
        #[cfg(not(unix))]
        {
            let (sender, receiver) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                let _ = sender.send(tokio::signal::ctrl_c().await);
            });
            Ok(Self { ctrl_c: receiver })
        }
    }

    async fn wait(self) -> Result<()> {
        #[cfg(unix)]
        {
            let mut interrupt = self.interrupt;
            let mut terminate = self.terminate;
            tokio::select! {
                _ = interrupt.recv() => {},
                _ = terminate.recv() => {},
            }
        }
        #[cfg(not(unix))]
        self.ctrl_c
            .await
            .map_err(|_| std::io::Error::other("Ctrl-C signal handler stopped"))??;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn authorization_rejects_missing_malformed_and_duplicate_headers() {
        let dir = tempfile::tempdir().unwrap();
        let token = auth::LocalToken::from_data_dir(dir.path()).unwrap();
        let token_text =
            std::fs::read_to_string(dir.path().join("credentials/local-api-token")).unwrap();
        let bearer = format!("Bearer {}", token_text.trim());

        let empty = HeaderMap::new();
        assert!(!authorized(&empty, &token));

        let mut malformed = HeaderMap::new();
        malformed.insert(AUTHORIZATION, HeaderValue::from_static("Basic abc"));
        assert!(!authorized(&malformed, &token));

        let mut duplicate = HeaderMap::new();
        duplicate.append(AUTHORIZATION, HeaderValue::from_str(&bearer).unwrap());
        duplicate.append(AUTHORIZATION, HeaderValue::from_str(&bearer).unwrap());
        assert!(!authorized(&duplicate, &token));

        let mut valid = HeaderMap::new();
        valid.insert(AUTHORIZATION, HeaderValue::from_str(&bearer).unwrap());
        assert!(authorized(&valid, &token));
    }
}
