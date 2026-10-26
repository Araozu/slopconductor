use std::io::{self, IsTerminal};

use hive_client::DaemonClient;
use hive_protocol::chat::CreateSessionRequest;
use sha2::{Digest, Sha256};

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
        full_context,
        delivery,
        workspace_id,
        no_workspace,
        images,
    } = args;
    let input = prompt_value_optional(prompt, prompt_file).await?;
    let interactive = input.is_none() && io::stdin().is_terminal();
    if input.is_none() && !interactive {
        return Err(
            "chat needs --prompt, --prompt-file, piped stdin, or an interactive terminal".into(),
        );
    }
    let client = context.client()?;
    let attachments = crate::images::load_image_attachments(&client, &images).await?;
    let settings = if effort.is_some() || max_output_tokens.is_some() || full_context.is_some() {
        Some(hive_protocol::execution::GenerationSettings {
            max_output_tokens,
            reasoning_effort: effort,
            full_context,
        })
    } else {
        None
    };
    let selection = if session.is_some() {
        TurnSelection {
            model: model.clone(),
            settings: settings.clone(),
            delivery: delivery.map(|value| match value.as_str() {
                "after-turn" => hive_protocol::chat::DeliveryMode::AfterTurn,
                "after-finish" => hive_protocol::chat::DeliveryMode::AfterFinish,
                "next-boundary" => hive_protocol::chat::DeliveryMode::NextBoundary,
                "immediate" => hive_protocol::chat::DeliveryMode::Immediate,
                _ => unreachable!("CLI validates delivery mode"),
            }),
            images: Vec::new(),
        }
    } else {
        TurnSelection::default()
    };
    let base_command_id = if session.is_none() {
        Some(choose_command_id(command_id.clone())?)
    } else {
        None
    };
    let workspace = match (workspace, workspace_id, no_workspace) {
        (Some(path), None, false) => {
            if !path.is_absolute() {
                return Err("workspace path must be absolute on the daemon's machine".into());
            }
            let base_id = base_command_id.as_deref().expect("new chat has command ID");
            let open_id = derived_command_id(base_id, "workspace");
            let opened = client
                .open_workspace(&hive_protocol::workspaces::OpenWorkspaceRequest {
                    command_id: open_id.clone(),
                    path: path.to_str().ok_or("workspace path must be UTF-8")?.into(),
                })
                .await
                .map_err(|error| mutation_error(error, base_id))?;
            Some(opened.id)
        }
        (None, Some(id), false) => Some(id),
        (None, None, _) => Some(hive_protocol::workspaces::DEFAULT_WORKSPACE_ID.into()),
        _ => unreachable!("CLI enforces exclusive workspace selection"),
    };
    let workspace_id = workspace.expect("every session has a workspace");
    if workspace_id == hive_protocol::workspaces::DEFAULT_WORKSPACE_ID && !tools.is_empty() {
        return Err("the default workspace has no directory and cannot enable tools".into());
    }
    let workspace = hive_protocol::workspaces::WorkspaceSelection {
        allowed_tools: if tools.is_empty()
            && workspace_id != hive_protocol::workspaces::DEFAULT_WORKSPACE_ID
        {
            vec!["read".into(), "write".into(), "edit".into(), "bash".into()]
        } else {
            tools.clone()
        },
        workspace_id,
        limits: None,
    };
    let mut session_id = match session {
        Some(id) => id,
        None => {
            create_session(
                &client,
                model,
                base_command_id.as_deref(),
                context.output,
                settings,
                workspace,
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
    let mut pending_images = Some(attachments);
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
        let mut current = selection.clone();
        // --image flags attach to the first message only.
        current.images = pending_images.take().unwrap_or_default();
        match send_and_follow(
            &client,
            &session_id,
            message,
            id,
            detach,
            context.output,
            current,
        )
        .await
        {
            Ok(()) => (),
            Err(error) if interactive => eprintln!("hivectl: {error}"),
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
    settings: Option<hive_protocol::execution::GenerationSettings>,
    workspace: hive_protocol::workspaces::WorkspaceSelection,
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
    let base_command_id = command_id.ok_or("new chat command ID was not prepared")?;
    let request = CreateSessionRequest {
        command_id: session_creation_command_id(base_command_id),
        title: None,
        provider,
        model,
        max_tokens: None,
        settings,
        workspace,
        ..Default::default()
    };
    let receipt = client
        .create_session(&request)
        .await
        .map_err(|error| mutation_error(error, base_command_id))?;
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
    derived_command_id(base, "session")
}

fn derived_command_id(base: &str, operation: &str) -> String {
    let candidate = format!("{base}:{operation}");
    if candidate.len() <= 128 {
        return candidate;
    }
    let digest = Sha256::digest([base.as_bytes(), b"\0", operation.as_bytes()].concat());
    format!("derived-{operation}-{:x}", digest)
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
            derived_command_id(base, "session")
        );
        assert_ne!(session_creation_command_id(base), base);
        let long = "x".repeat(128);
        let workspace_id = derived_command_id(&long, "workspace");
        assert!(workspace_id.len() <= 128);
        assert_eq!(workspace_id, derived_command_id(&long, "workspace"));
        assert_ne!(workspace_id, derived_command_id(&long, "session"));
    }
}
