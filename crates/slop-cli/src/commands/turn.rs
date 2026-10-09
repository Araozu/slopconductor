use slop_client::DaemonClient;
use slop_protocol::chat::CancelTurnRequest;

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
        TurnCommand::Show { id } => {
            let turn = client.turn(&id).await?;
            context.output.value(&turn, || {
                println!("{}\t{}\t{}", turn.id, turn.status, turn.requested_model)
            })?;
        }
        TurnCommand::Cancel { id, command_id } => {
            cancel_turn(&client, &id, command_id, context.output).await?;
        }
    }
    Ok(())
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
