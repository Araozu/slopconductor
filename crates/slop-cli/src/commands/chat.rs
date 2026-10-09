use std::io::{self, IsTerminal};

use slop_client::DaemonClient;
use slop_protocol::chat::CreateSessionRequest;

use crate::{
    Result,
    args::ChatArgs,
    follow::{follow_turn, is_terminal},
    history::print_history,
    input::{PromptInput, prompt_value_optional, read_interactive_prompt},
    mutation::{choose_command_id, mutation_error},
    output::Output,
};

use super::{
    Context,
    session::{TurnSelection, send_and_follow},
    turn::cancel_turn,
};

pub(super) async fn run(args: ChatArgs, context: &Context) -> Result<()> {
    let ChatArgs {
        session,
        model,
        prompt,
        prompt_file,
        command_id,
        detach,
        workspace,
        tools,
        effort,
        max_output_tokens,
    } = args;
    let input = prompt_value_optional(prompt, prompt_file).await?;
    let interactive = input.is_none() && io::stdin().is_terminal();
    if input.is_none() && !interactive {
        return Err(
            "chat needs --prompt, --prompt-file, piped stdin, or an interactive terminal".into(),
        );
    }
    let client = context.client()?;
    let settings = if effort.is_some() || max_output_tokens.is_some() {
        Some(slop_protocol::execution::GenerationSettings {
            max_output_tokens,
            reasoning_effort: effort,
        })
    } else {
        None
    };
    let selection = if session.is_some() {
        TurnSelection {
            model: model.clone(),
            settings: settings.clone(),
        }
    } else {
        TurnSelection::default()
    };
    let execution = workspace
        .map(|root| {
            std::path::absolute(root).map(|root| slop_protocol::execution::WorkspacePolicy {
                root: root.to_string_lossy().into_owned(),
                allowed_tools: if tools.is_empty() {
                    vec!["read".into(), "write".into(), "edit".into(), "bash".into()]
                } else {
                    tools
                },
                shell_timeout_ms: 30_000,
                max_output_bytes: 1024 * 1024,
                max_tool_calls: 32,
                max_model_requests: 16,
            })
        })
        .transpose()?;
    let mut session_id = match session {
        Some(id) => id,
        None => {
            create_session(
                &client,
                model,
                command_id.as_deref(),
                context.output,
                settings,
                execution,
            )
            .await?
        }
    };

    let mut interactive_stdin = interactive.then(|| tokio::io::BufReader::new(tokio::io::stdin()));
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
                    context.output,
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
                        cancel_turn(&client, &turn_id, None, context.output).await?;
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
        match send_and_follow(
            &client,
            &session_id,
            message,
            id,
            detach,
            context.output,
            selection.clone(),
        )
        .await
        {
            Ok(()) => (),
            Err(error) if interactive => eprintln!("slop: {error}"),
            Err(error) => return Err(error),
        }
        if detach || !interactive {
            break;
        }
        session_id = client.session(&session_id).await?.id;
    }
    Ok(())
}

async fn create_session(
    client: &DaemonClient,
    model: Option<String>,
    command_id: Option<&str>,
    output: Output,
    settings: Option<slop_protocol::execution::GenerationSettings>,
    execution: Option<slop_protocol::execution::WorkspacePolicy>,
) -> Result<String> {
    let (provider, model) = match model {
        Some(value) => parse_model(&value)?,
        None => {
            let choices = client.models().await?;
            let first = choices
                .into_iter()
                .find(|candidate| candidate.is_default && candidate.ready)
                .ok_or("daemon has no ready default model; pass --model provider/model")?;
            (first.provider, first.model)
        }
    };
    let base_command_id = choose_command_id(command_id.map(str::to_owned))?;
    let request = CreateSessionRequest {
        command_id: session_creation_command_id(&base_command_id),
        title: None,
        provider,
        model,
        max_tokens: None,
        settings,
        execution,
    };
    let receipt = client
        .create_session(&request)
        .await
        .map_err(|error| mutation_error(error, &base_command_id))?;
    output.session_receipt(&receipt);
    let session_id = receipt.session_id;
    eprintln!("session {session_id}");
    Ok(session_id)
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

fn session_creation_command_id(base: &str) -> String {
    format!("{base}:session")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_syntax_requires_both_provider_and_model() {
        assert_eq!(
            parse_model("opencode-go/glm-5.3-flash").unwrap(),
            ("opencode-go".into(), "glm-5.3-flash".into())
        );
        for value in ["missing-provider-separator", "/model", "provider/"] {
            assert!(parse_model(value).is_err(), "accepted {value:?}");
        }
    }

    #[test]
    fn a_new_chat_retries_creation_with_a_stable_distinct_command_id() {
        let base = "command-0123456789abcdef";
        assert_eq!(
            session_creation_command_id(base),
            "command-0123456789abcdef:session"
        );
        assert_ne!(session_creation_command_id(base), base);
    }
}
