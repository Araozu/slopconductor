use slop_client::DaemonClient;
use slop_protocol::chat::{CancelTurnRequest, TurnControlRequest};

use crate::{
    Result,
    args::TurnCommand,
    mutation::{choose_command_id, mutation_error},
    output::Output,
};

use super::Context;

pub(super) async fn run(command: TurnCommand, context: &Context) -> Result<()> {
    let client = context.client()?;
    match command {
        TurnCommand::Requests { id, after, limit } => {
            let page = client.model_requests(&id, after, Some(limit)).await?;
            context.output.value(&page, || {
                for request in &page.items {
                    println!(
                        "{}\t{}\t{}\t{}",
                        request.id,
                        request.status,
                        request.requested_model,
                        request.finish_reason.as_deref().unwrap_or("pending")
                    );
                }
            })?;
        }
        TurnCommand::Tools { id, after, limit } => {
            let page = client.tool_invocations(&id, after, Some(limit)).await?;
            context.output.value(&page, || {
                for tool in &page.items {
                    println!("{}\t{}\t{}", tool.id, tool.name, tool.status);
                    if let Some(output) = &tool.output {
                        println!("{output}");
                    }
                    for artifact in &tool.artifact_ids {
                        println!("artifact {artifact}");
                    }
                }
            })?;
        }
        TurnCommand::Show { id } => {
            let turn = client.turn(&id).await?;
            context.output.value(&turn, || {
                println!("{}\t{}\t{}", turn.id, turn.status, turn.requested_model)
            })?;
        }
        TurnCommand::Cancel { id, command_id } => {
            cancel_turn(&client, &id, command_id, context.output).await?;
        }
        TurnCommand::Pause { id, command_id } => {
            control_turn(&client, &id, command_id, context.output, true).await?;
        }
        TurnCommand::Resume { id, command_id } => {
            control_turn(&client, &id, command_id, context.output, false).await?;
        }
    }
    Ok(())
}

async fn control_turn(
    client: &DaemonClient,
    turn_id: &str,
    command_id: Option<String>,
    output: Output,
    pause: bool,
) -> Result<()> {
    let command_id = choose_command_id(command_id)?;
    let request = TurnControlRequest {
        command_id: command_id.clone(),
    };
    let result = if pause {
        client.pause_turn(turn_id, &request).await
    } else {
        client.resume_turn(turn_id, &request).await
    };
    let receipt = result.map_err(|error| mutation_error(error, &command_id))?;
    output.value(&receipt, || {
        println!(
            "{} accepted for turn {turn_id}",
            if pause { "pause" } else { "resume" }
        )
    })
}

pub(super) async fn cancel_turn(
    client: &DaemonClient,
    turn_id: &str,
    command_id: Option<String>,
    output: Output,
) -> Result<()> {
    let command_id = choose_command_id(command_id)?;
    let request = CancelTurnRequest {
        command_id: command_id.clone(),
    };
    let receipt = client
        .cancel_turn(turn_id, &request)
        .await
        .map_err(|error| mutation_error(error, &command_id))?;
    output.value(&receipt, || println!("cancel accepted for turn {turn_id}"))
}
