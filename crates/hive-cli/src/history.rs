use hive_client::{ClientError, DaemonClient};
use hive_protocol::chat::MessageResponse;

use crate::Result;

pub async fn print_history(client: &DaemonClient, session_id: &str) -> Result<Option<String>> {
    let mut after = None;
    let mut latest_turn = None;
    loop {
        let page = client.history(session_id, after, Some(100)).await?;
        for message in &page.items {
            latest_turn = Some(message.turn_id.clone());
            if !matches!(message.status.as_str(), "checkpoint" | "pending") {
                println!("{}: {}", message.role, message.text);
                for block in &message.blocks {
                    if let hive_protocol::execution::BlockContent::ToolCall {
                        name, call_id, ..
                    } = &block.content
                    {
                        println!("  {name} requested ({call_id})");
                    }
                }
            }
        }
        match page.next_after {
            Some(next) => after = Some(next),
            None => break,
        }
    }
    Ok(latest_turn)
}

pub async fn find_message(
    client: &DaemonClient,
    session_id: &str,
    id: &str,
) -> Result<Option<MessageResponse>> {
    match client.message(id).await {
        Ok(message) if message.session_id == session_id => Ok(Some(message)),
        Ok(_) | Err(ClientError::Api { status: 404, .. }) => Ok(None),
        Err(error) => Err(error.into()),
    }
}
