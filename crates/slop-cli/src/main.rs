use std::{
    error::Error,
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

use clap::{Parser, Subcommand};
use slop_client::{ClientError, DaemonClient, EventStream, new_command_id};
use slop_protocol::{DEFAULT_DAEMON_URL, chat::*};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;
const MAX_PROMPT_BYTES: usize = 64 * 1024;
const MAX_RENDERED_TURN_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Parser)]
#[command(name = "slop", version, about = "Slop Conductor command-line client")]
struct Args {
    /// Daemon origin to connect to.
    #[arg(long, global = true, env = "SLOP_DAEMON_URL", default_value = DEFAULT_DAEMON_URL)]
    daemon: String,

    /// Emit machine-readable JSON on standard output.
    #[arg(long, global = true)]
    json: bool,

    /// Read the local API bearer token from this file.
    #[arg(long, global = true)]
    token_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Subcommand)]
enum Command {
    /// Check daemon connectivity, API compatibility, and capabilities.
    Status,
    /// Show this daemon's authenticated node identity.
    Node,
    /// Discover available models.
    Models,
    /// Manage conversations.
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Inspect or cancel an execution turn.
    Turn {
        #[command(subcommand)]
        command: TurnCommand,
    },
    /// Start or resume a text conversation.
    Chat {
        #[arg(long)]
        session: Option<String>,
        #[arg(long, conflicts_with = "session")]
        model: Option<String>,
        #[arg(long, conflicts_with = "prompt_file")]
        prompt: Option<String>,
        #[arg(long)]
        prompt_file: Option<PathBuf>,
        #[arg(long)]
        command_id: Option<String>,
        /// Return after durable acceptance without following the turn.
        #[arg(long)]
        detach: bool,
    },
}

#[derive(Debug, Clone, Subcommand)]
enum SessionCommand {
    List {
        #[arg(long)]
        after: Option<u64>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    Show {
        id: String,
    },
    History {
        id: String,
        #[arg(long)]
        after: Option<u64>,
        #[arg(long, default_value_t = 100)]
        limit: u32,
    },
    Send {
        id: String,
        #[arg(long, conflicts_with = "text")]
        prompt_file: Option<PathBuf>,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        command_id: Option<String>,
        #[arg(long)]
        detach: bool,
    },
    Follow {
        id: String,
        #[arg(long, default_value_t = 0)]
        after: u64,
    },
}

#[derive(Debug, Clone, Subcommand)]
enum TurnCommand {
    Show {
        id: String,
    },
    Cancel {
        id: String,
        #[arg(long)]
        command_id: Option<String>,
    },
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
    match args.command.clone() {
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
            let client = authenticated_client(&args)?;
            let node = client.node().await?;
            if args.json {
                println!("{}", serde_json::to_string(&node)?);
            } else {
                println!("{} ({})", node.name, node.node_id);
                println!("OS: {}", node.os);
            }
        }
        Command::Models => {
            let client = authenticated_client(&args)?;
            let models = client.models().await?;
            emit_value(&models, args.json, || {
                for model in &models {
                    println!(
                        "{}\t{}\t{}",
                        if model.ready { "ready" } else { "unavailable" },
                        model.id,
                        model.display_name
                    );
                    if let Some(reason) = &model.reason {
                        println!("  {reason}");
                    }
                }
            })?;
        }
        Command::Session { command } => {
            let client = authenticated_client(&args)?;
            match command {
                SessionCommand::List { after, limit } => {
                    let page = client.list_sessions(after, Some(limit)).await?;
                    emit_value(&page, args.json, || {
                        for session in &page.items {
                            println!(
                                "{}\t{}\t{}",
                                session.id,
                                session.model,
                                session.title.as_deref().unwrap_or_default()
                            );
                        }
                    })?;
                }
                SessionCommand::Show { id } => {
                    let session = client.session(&id).await?;
                    emit_value(&session, args.json, || {
                        println!(
                            "{}\t{}\t{}",
                            session.id,
                            session.model,
                            session.title.as_deref().unwrap_or_default()
                        );
                        println!(
                            "Revision: {}  Event: {}",
                            session.revision, session.last_event_sequence
                        );
                    })?;
                }
                SessionCommand::History { id, after, limit } => {
                    let page = client.history(&id, after, Some(limit)).await?;
                    emit_value(&page, args.json, || {
                        for message in &page.items {
                            println!("{}: {}", message.role, message.text);
                        }
                    })?;
                }
                SessionCommand::Send {
                    id,
                    prompt_file,
                    text,
                    command_id,
                    detach,
                } => {
                    let text = prompt_value(text, prompt_file).await?;
                    let command_id = choose_command_id(command_id)?;
                    send_and_follow(&client, &id, text, command_id, detach, args.json).await?;
                }
                SessionCommand::Follow { id, after } => {
                    follow_session(&client, &id, after, args.json).await?;
                }
            }
        }
        Command::Turn { command } => {
            let client = authenticated_client(&args)?;
            match command {
                TurnCommand::Show { id } => {
                    let turn = client.turn(&id).await?;
                    emit_value(&turn, args.json, || {
                        println!("{}\t{}\t{}", turn.id, turn.status, turn.requested_model)
                    })?;
                }
                TurnCommand::Cancel { id, command_id } => {
                    cancel_turn(&client, &id, command_id, args.json).await?;
                }
            }
        }
        Command::Chat {
            session,
            model,
            prompt,
            prompt_file,
            command_id,
            detach,
        } => {
            let input = prompt_value_optional(prompt, prompt_file).await?;
            let interactive = input.is_none() && io::stdin().is_terminal();
            if input.is_none() && !interactive {
                return Err(
                    "chat needs --prompt, --prompt-file, piped stdin, or an interactive terminal"
                        .into(),
                );
            }
            let client = authenticated_client(&args)?;
            let mut session_id = match session {
                Some(id) => id,
                None => {
                    let selected = match model {
                        Some(value) => parse_model(&value)?,
                        None => {
                            let choices = client.models().await?;
                            let first = choices.into_iter().find(|candidate| candidate.is_default && candidate.ready)
                                .ok_or("daemon has no ready default model; pass --model provider/model")?;
                            (first.provider, first.model)
                        }
                    };
                    let base_command_id = match &command_id {
                        Some(value) => value.clone(),
                        None => new_command_id()?,
                    };
                    let create_command_id = session_creation_command_id(&base_command_id);
                    let request = CreateSessionRequest {
                        command_id: create_command_id.clone(),
                        title: None,
                        provider: selected.0,
                        model: selected.1,
                        max_tokens: None,
                    };
                    let receipt = client
                        .create_session(&request)
                        .await
                        .map_err(|error| mutation_error(error, &base_command_id))?;
                    if args.json {
                        println!(
                            "{}",
                            serde_json::json!({"type":"session_receipt","receipt":receipt})
                        );
                    }
                    let session_id = receipt.session_id;
                    eprintln!("session {session_id}");
                    session_id
                }
            };

            let mut interactive_stdin =
                interactive.then(|| tokio::io::BufReader::new(tokio::io::stdin()));
            if interactive {
                let latest_turn = print_history(&client, &session_id).await?;
                if let Some(turn_id) = latest_turn {
                    let turn = client.turn(&turn_id).await?;
                    if !is_terminal(&turn.status) {
                        let session_state = client.session(&session_id).await?;
                        follow_turn(
                            &client,
                            &session_id,
                            &turn_id,
                            session_state.last_event_sequence,
                            args.json,
                        )
                        .await?;
                    }
                }
            }
            let mut next_id = command_id;
            let mut first = input;
            loop {
                let message = match first.take() {
                    Some(text) => text,
                    None if interactive => match read_interactive_prompt(
                        interactive_stdin
                            .as_mut()
                            .expect("interactive stdin exists"),
                    )
                    .await?
                    {
                        PromptInput::Exit => break,
                        PromptInput::Cancel(turn_id) => {
                            if let Some(turn_id) = turn_id {
                                cancel_turn(&client, &turn_id, None, args.json).await?;
                            } else {
                                eprintln!("usage: /cancel TURN_ID");
                            }
                            continue;
                        }
                        PromptInput::Text(text) => text,
                    },
                    None => break,
                };
                let id = choose_command_id(next_id.take())?;
                match send_and_follow(&client, &session_id, message, id, detach, args.json).await {
                    Ok(()) => (),
                    Err(error) if interactive => eprintln!("slop: {error}"),
                    Err(error) => return Err(error),
                }
                if detach || !interactive {
                    break;
                }
                session_id = client.session(&session_id).await?.id;
            }
        }
    }
    Ok(())
}

async fn send_and_follow(
    client: &DaemonClient,
    session_id: &str,
    text: String,
    command_id: String,
    detach: bool,
    json: bool,
) -> Result<()> {
    let request = SendMessageRequest {
        command_id: command_id.clone(),
        text,
        expected_revision: None,
    };
    let receipt = client
        .send_message(session_id, &request)
        .await
        .map_err(|error| mutation_error(error, &command_id))?;
    if json {
        println!(
            "{}",
            serde_json::json!({"type":"receipt","receipt":receipt})
        );
    } else if detach {
        println!(
            "accepted session {} turn {} command {}",
            receipt.session_id,
            receipt.turn_id.as_deref().unwrap_or("pending"),
            receipt.command_id
        );
    }
    if detach {
        return Ok(());
    }
    let Some(turn_id) = receipt.turn_id.as_deref() else {
        return Ok(());
    };
    follow_turn(client, session_id, turn_id, receipt.event_sequence, json).await
}

async fn follow_turn(
    client: &DaemonClient,
    session_id: &str,
    turn_id: &str,
    mut cursor: u64,
    json: bool,
) -> Result<()> {
    let mut shown = String::new();
    let mut attempts = 0_u32;
    let mut terminal_turn = None;
    loop {
        match client.events(session_id, cursor, true).await {
            Ok(mut stream) => {
                match consume_stream(
                    &mut stream,
                    &mut cursor,
                    turn_id,
                    &mut shown,
                    &mut terminal_turn,
                    json,
                )
                .await
                {
                    Ok(true) => return Ok(()),
                    Ok(false) => attempts += 1,
                    Err(error) => {
                        attempts += 1;
                        if attempts > 8 {
                            return Err(error);
                        }
                    }
                }
            }
            Err(error) => {
                attempts += 1;
                if attempts > 8 {
                    return Err(error.into());
                }
            }
        }
        let turn = client.turn(turn_id).await?;
        if is_terminal(&turn.status) {
            let canonical = if let Some(id) = &turn.assistant_message_id {
                find_message(client, session_id, id).await?
            } else {
                None
            };
            if json {
                println!(
                    "{}",
                    serde_json::json!({"type":"terminal","turn":turn,"message":canonical})
                );
            } else if let Some(message) = canonical {
                if !message.text.is_empty() {
                    if shown.is_empty() {
                        println!("{}", message.text);
                    } else if message.text.starts_with(&shown) {
                        print!("{}", &message.text[shown.len()..]);
                        println!();
                    } else {
                        eprintln!("\n[canonical assistant reply updated]");
                        println!("{}", message.text);
                    }
                }
                if turn.status != "completed" {
                    eprintln!("turn {}", turn.status);
                }
            } else if turn.status != "completed" {
                eprintln!("turn {}", turn.status);
            }
            if turn.status != "completed" {
                return Err(format!(
                    "turn {} ended with status {}: {}",
                    turn.id,
                    turn.status,
                    turn.error_message
                        .as_deref()
                        .unwrap_or("no additional details")
                )
                .into());
            }
            return Ok(());
        }
        attempts += 1;
        if attempts > 8 {
            return Err(format!("turn {turn_id} is still running; reconnect with `slop session follow {session_id} --after {cursor}`").into());
        }
        tokio::time::sleep(Duration::from_millis(
            250_u64.saturating_mul(1 << attempts.min(3)),
        ))
        .await;
    }
}

async fn consume_stream(
    stream: &mut EventStream,
    cursor: &mut u64,
    turn_id: &str,
    shown: &mut String,
    terminal_turn: &mut Option<String>,
    json: bool,
) -> Result<bool> {
    loop {
        let frame = match tokio::select! {
            _ = tokio::signal::ctrl_c() => return Ok(true),
            frame = stream.next_frame() => frame,
        } {
            Ok(Some(frame)) => frame,
            Ok(None) => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        match &frame {
            EventFrame::Durable { event } => {
                *cursor = (*cursor).max(event.sequence);
                if (turn_id.is_empty() || event.turn_id.as_deref() == Some(turn_id))
                    && matches!(
                        event.kind.as_str(),
                        "turn_completed"
                            | "turn_failed"
                            | "turn_cancelled"
                            | "turn_interrupted"
                            | "turn_incomplete"
                    )
                {
                    *terminal_turn = event.turn_id.clone();
                }
            }
            EventFrame::Delta {
                turn_id: frame_turn,
                text,
                ..
            } if turn_id.is_empty() || frame_turn == turn_id => {
                let remaining = MAX_RENDERED_TURN_BYTES.saturating_sub(shown.len());
                let visible_len = bounded_utf8_prefix_len(text, remaining);
                let visible = &text[..visible_len];
                shown.push_str(visible);
                if !json {
                    print!("{visible}");
                    io::stdout().flush()?;
                }
            }
            EventFrame::Delta { .. } | EventFrame::Heartbeat => (),
        }
        if json {
            println!("{}", serde_json::to_string(&frame)?);
        }
        if terminal_turn.is_some() {
            return Ok(false);
        }
    }
}

fn bounded_utf8_prefix_len(text: &str, max_bytes: usize) -> usize {
    let mut end = 0;
    for (start, character) in text.char_indices() {
        let next = start + character.len_utf8();
        if next > max_bytes {
            break;
        }
        end = next;
    }
    end
}

async fn follow_session(
    client: &DaemonClient,
    session_id: &str,
    mut cursor: u64,
    json: bool,
) -> Result<()> {
    let mut shown = String::new();
    let mut attempts = 0_u32;
    let mut terminal_turn = None;
    loop {
        match client.events(session_id, cursor, true).await {
            Ok(mut stream) => {
                match consume_stream(
                    &mut stream,
                    &mut cursor,
                    "",
                    &mut shown,
                    &mut terminal_turn,
                    json,
                )
                .await
                {
                    Ok(true) => return Ok(()),
                    Ok(false) if terminal_turn.is_some() => {
                        let turn_id = terminal_turn.take().expect("checked terminal turn");
                        let turn = client.turn(&turn_id).await?;
                        let message = if let Some(id) = &turn.assistant_message_id {
                            find_message(client, session_id, id).await?
                        } else {
                            None
                        };
                        if json {
                            println!(
                                "{}",
                                serde_json::json!({"type":"canonical","turn":turn,"message":message})
                            );
                        } else if let Some(message) = message {
                            if message.text.starts_with(&shown) {
                                print!("{}", &message.text[shown.len()..]);
                                println!();
                            } else if shown.is_empty() {
                                println!("{}", message.text);
                            } else {
                                eprintln!("\n[canonical assistant reply updated]");
                                println!("{}", message.text);
                            }
                        }
                        shown.clear();
                        attempts = 0;
                    }
                    Ok(false) => attempts += 1,
                    Err(error) => {
                        attempts += 1;
                        eprintln!(
                            "event stream disconnected; reconnecting after durable sequence {cursor}: {error}"
                        );
                    }
                }
            }
            Err(error) => {
                attempts += 1;
                eprintln!(
                    "event stream disconnected; reconnecting after durable sequence {cursor}: {error}"
                );
            }
        }
        if attempts > 8 {
            return Err("event stream could not reconnect after repeated failures".into());
        }
        tokio::time::sleep(Duration::from_millis(
            250_u64.saturating_mul(1 << attempts.min(3)),
        ))
        .await;
    }
}

async fn print_history(client: &DaemonClient, session_id: &str) -> Result<Option<String>> {
    let mut after = None;
    let mut latest_turn = None;
    loop {
        let page = client.history(session_id, after, Some(100)).await?;
        for message in &page.items {
            latest_turn = Some(message.turn_id.clone());
            if message.status != "checkpoint" {
                println!("{}: {}", message.role, message.text);
            }
        }
        match page.next_after {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    Ok(latest_turn)
}

async fn find_message(
    client: &DaemonClient,
    session_id: &str,
    id: &str,
) -> Result<Option<MessageResponse>> {
    let mut after = None;
    loop {
        let page = client.history(session_id, after, Some(100)).await?;
        if let Some(message) = page.items.into_iter().find(|message| message.id == id) {
            return Ok(Some(message));
        }
        match page.next_after {
            Some(next) => after = Some(next),
            None => return Ok(None),
        }
    }
}

async fn read_interactive_prompt(
    stdin: &mut tokio::io::BufReader<tokio::io::Stdin>,
) -> Result<PromptInput> {
    eprint!("you> ");
    io::stderr().flush()?;
    let mut line = String::new();
    use tokio::io::AsyncBufReadExt;
    let read = stdin.read_line(&mut line).await?;
    if read == 0 || line.trim() == "/exit" {
        return Ok(PromptInput::Exit);
    }
    if let Some(rest) = line.trim().strip_prefix("/cancel") {
        return Ok(PromptInput::Cancel(
            (!rest.trim().is_empty()).then(|| rest.trim().to_owned()),
        ));
    }
    Ok(PromptInput::Text(validate_prompt(
        line.trim_end().to_owned(),
    )?))
}

enum PromptInput {
    Exit,
    Cancel(Option<String>),
    Text(String),
}

async fn prompt_value(text: Option<String>, path: Option<PathBuf>) -> Result<String> {
    prompt_value_optional(text, path)
        .await?
        .ok_or_else(|| "a prompt is required".into())
}

async fn prompt_value_optional(
    text: Option<String>,
    path: Option<PathBuf>,
) -> Result<Option<String>> {
    let value = if let Some(text) = text {
        Some(text)
    } else if let Some(path) = path {
        Some(read_bounded_text(&path)?)
    } else if !io::stdin().is_terminal() {
        Some(read_bounded_stdin().await?)
    } else {
        None
    };
    value.map(validate_prompt).transpose()
}

fn read_bounded_text(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut bytes = Vec::with_capacity(MAX_PROMPT_BYTES.min(4096));
    std::fs::File::open(path)
        .and_then(|file| {
            file.take((MAX_PROMPT_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
        })
        .map_err(|error| format!("could not read prompt file {}: {error}", path.display()))?;
    let value = String::from_utf8(bytes).map_err(|_| "prompt input is not UTF-8")?;
    validate_prompt(value)
}

async fn read_bounded_stdin() -> Result<String> {
    let mut bytes = Vec::new();
    use tokio::io::AsyncReadExt;
    tokio::io::stdin()
        .take((MAX_PROMPT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .await?;
    let value = String::from_utf8(bytes).map_err(|_| "prompt input is not UTF-8")?;
    validate_prompt(value)
}

fn parse_model(value: &str) -> Result<(String, String)> {
    let (provider, model) = value
        .split_once('/')
        .ok_or("model must use provider/model syntax")?;
    if provider.is_empty() || model.is_empty() {
        return Err("model must use provider/model syntax".into());
    }
    Ok((provider.to_owned(), model.to_owned()))
}

fn validate_prompt(value: String) -> Result<String> {
    if value.len() > MAX_PROMPT_BYTES {
        Err("prompt exceeds the 64 KiB input limit".into())
    } else {
        Ok(value)
    }
}

fn session_creation_command_id(base: &str) -> String {
    format!("{base}:session")
}

fn authenticated_client(args: &Args) -> Result<DaemonClient> {
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
        return Ok(std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .map(|root| {
                root.join("slopconductor")
                    .join("credentials")
                    .join("local-api-token")
            }));
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

fn mutation_error(error: ClientError, command_id: &str) -> Box<dyn Error + Send + Sync> {
    match error {
        ClientError::DeliveryUncertain(message) => {
            format!("{message}; retry the same operation with --command-id {command_id}").into()
        }
        other => Box::new(other),
    }
}

fn choose_command_id(value: Option<String>) -> Result<String> {
    match value {
        Some(value) => Ok(value),
        None => Ok(new_command_id()?),
    }
}

async fn cancel_turn(
    client: &DaemonClient,
    turn_id: &str,
    command_id: Option<String>,
    json: bool,
) -> Result<()> {
    let command_id = choose_command_id(command_id)?;
    let request = CancelTurnRequest {
        command_id: command_id.clone(),
    };
    let receipt = client
        .cancel_turn(turn_id, &request)
        .await
        .map_err(|error| mutation_error(error, &command_id))?;
    emit_value(&receipt, json, || {
        println!("cancel accepted for turn {turn_id}")
    })
}

fn emit_value<T: serde::Serialize>(value: &T, json: bool, human: impl FnOnce()) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string(value)?);
    } else {
        human();
    }
    Ok(())
}

fn is_terminal(status: &str) -> bool {
    matches!(
        status,
        "completed"
            | "failed"
            | "cancelled"
            | "canceled"
            | "interrupted"
            | "incomplete"
            | "rejected"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_limit_and_model_syntax_are_checked() {
        assert_eq!(
            validate_prompt("x".repeat(MAX_PROMPT_BYTES)).unwrap().len(),
            MAX_PROMPT_BYTES
        );
        assert!(validate_prompt("x".repeat(MAX_PROMPT_BYTES + 1)).is_err());
        assert_eq!(bounded_utf8_prefix_len("a☃b", 4), "a☃".len());
        assert_eq!(
            parse_model("opencode-go/glm-5.3-flash").unwrap(),
            ("opencode-go".into(), "glm-5.3-flash".into())
        );
        assert!(parse_model("missing-provider-separator").is_err());
    }

    #[test]
    fn a_new_chat_retries_creation_with_a_stable_distinct_command_id() {
        let base = "command-0123456789abcdef";
        assert_eq!(
            session_creation_command_id(base),
            "command-0123456789abcdef:session"
        );
        assert_ne!(session_creation_command_id(base), base);
        assert_eq!(
            session_creation_command_id(base),
            session_creation_command_id(base)
        );
    }

    #[test]
    fn model_cannot_be_silently_ignored_when_resuming_a_session() {
        let parsed = Args::try_parse_from([
            "slop",
            "chat",
            "--session",
            "session-1",
            "--model",
            "opencode-go/glm-5.3-flash",
            "--prompt",
            "hello",
        ]);
        assert!(parsed.is_err());
    }
}
