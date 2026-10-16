use slop_client::DaemonClient;
use slop_protocol::chat::{
    DeleteMessageRequest, EditMessageRequest, ForkSessionRequest, RenameSessionRequest,
    SendMessageRequest,
};

use crate::{
    Result,
    args::SessionCommand,
    follow::{follow_session, follow_turn},
    input::prompt_value,
    mutation::{choose_command_id, mutation_error},
    output::Output,
};

use super::Context;

#[derive(Default, Clone)]
pub(super) struct TurnSelection {
    pub model: Option<String>,
    pub settings: Option<slop_protocol::execution::GenerationSettings>,
    pub delivery: Option<slop_protocol::chat::DeliveryMode>,
}

pub(super) async fn run(command: SessionCommand, context: &Context) -> Result<()> {
    let client = context.client()?;
    match command {
        SessionCommand::List { after, limit } => {
            let page = client.list_sessions(after, Some(limit)).await?;
            context.output.value(&page, || {
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
            context.output.value(&session, || {
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
                println!("Workspace: {}", session.workspace_id);
                if let Some(policy) = &session.execution {
                    println!("Workspace path: {}", policy.root);
                }
            })?;
        }
        SessionCommand::Rename {
            id,
            title,
            command_id,
        } => {
            let command_id = choose_command_id(command_id)?;
            let receipt = client
                .rename_session(
                    &id,
                    &RenameSessionRequest {
                        command_id: command_id.clone(),
                        title,
                    },
                )
                .await
                .map_err(|error| mutation_error(error, &command_id))?;
            context
                .output
                .value(&receipt, || println!("renamed session {id}"))?;
        }
        SessionCommand::History { id, after, limit } => {
            let page = client.history(&id, after, Some(limit)).await?;
            context.output.value(&page, || {
                for message in &page.items {
                    println!("{}\t{}: {}", message.id, message.role, message.text);
                }
            })?;
        }
        SessionCommand::Fork {
            id,
            message,
            title,
            command_id,
            expected_revision,
        } => {
            let command_id = choose_command_id(command_id)?;
            let receipt = client
                .fork_session(
                    &id,
                    &ForkSessionRequest {
                        command_id: command_id.clone(),
                        message_id: message,
                        title,
                        expected_revision,
                    },
                )
                .await
                .map_err(|error| mutation_error(error, &command_id))?;
            context.output.value(&receipt, || {
                println!("forked session {}", receipt.session_id)
            })?;
        }
        SessionCommand::Edit {
            id,
            message,
            prompt_file,
            text,
            command_id,
            expected_revision,
            detach,
        } => {
            let text = prompt_value(text, prompt_file).await?;
            let command_id = choose_command_id(command_id)?;
            let receipt = client
                .edit_message(
                    &id,
                    &message,
                    &EditMessageRequest {
                        command_id: command_id.clone(),
                        text,
                        expected_revision,
                    },
                )
                .await
                .map_err(|error| mutation_error(error, &command_id))?;
            context.output.message_receipt(&receipt, true);
            if !detach && let Some(turn_id) = receipt.turn_id.as_deref() {
                follow_turn(
                    &client,
                    &receipt.session_id,
                    turn_id,
                    receipt.event_sequence,
                    context.output,
                )
                .await?;
            }
        }
        SessionCommand::DeleteMessage {
            id,
            message,
            command_id,
            expected_revision,
        } => {
            let command_id = choose_command_id(command_id)?;
            let receipt = client
                .delete_message(
                    &id,
                    &message,
                    &DeleteMessageRequest {
                        command_id: command_id.clone(),
                        expected_revision,
                    },
                )
                .await
                .map_err(|error| mutation_error(error, &command_id))?;
            context.output.value(&receipt, || {
                println!("deleted message suffix in session {}", receipt.session_id)
            })?;
        }
        SessionCommand::Send {
            id,
            prompt_file,
            text,
            command_id,
            detach,
            model,
            effort,
            max_output_tokens,
            full_context,
            delivery,
        } => {
            let text = prompt_value(text, prompt_file).await?;
            let command_id = choose_command_id(command_id)?;
            let selection = TurnSelection {
                model,
                delivery: delivery.map(|value| match value.as_str() {
                    "after-turn" => slop_protocol::chat::DeliveryMode::AfterTurn,
                    "after-finish" => slop_protocol::chat::DeliveryMode::AfterFinish,
                    "next-boundary" => slop_protocol::chat::DeliveryMode::NextBoundary,
                    "immediate" => slop_protocol::chat::DeliveryMode::Immediate,
                    _ => unreachable!("CLI validates delivery mode"),
                }),
                settings: if effort.is_some()
                    || max_output_tokens.is_some()
                    || full_context.is_some()
                {
                    Some(slop_protocol::execution::GenerationSettings {
                        max_output_tokens,
                        reasoning_effort: effort,
                        full_context,
                    })
                } else {
                    None
                },
            };
            send_and_follow(
                &client,
                &id,
                text,
                command_id,
                detach,
                context.output,
                selection,
            )
            .await?;
        }
        SessionCommand::Follow { id, after } => {
            follow_session(&client, &id, after, context.output).await?;
        }
    }
    Ok(())
}

pub(super) async fn send_and_follow(
    client: &DaemonClient,
    session_id: &str,
    text: String,
    command_id: String,
    detach: bool,
    output: Output,
    selection: TurnSelection,
) -> Result<()> {
    let request = SendMessageRequest {
        command_id: command_id.clone(),
        text,
        expected_revision: None,
        delivery: selection.delivery,
        model: selection.model,
        settings: selection.settings,
    };
    let receipt = client
        .send_message(session_id, &request)
        .await
        .map_err(|error| mutation_error(error, &command_id))?;
    output.message_receipt(&receipt, detach);
    if detach {
        return Ok(());
    }
    let Some(turn_id) = receipt.turn_id.as_deref() else {
        return Ok(());
    };
    follow_turn(client, session_id, turn_id, receipt.event_sequence, output).await
}
