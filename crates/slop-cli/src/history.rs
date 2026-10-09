use slop_client::DaemonClient;
use slop_protocol::chat::MessageResponse;

use crate::Result;

pub async fn print_history(client: &DaemonClient, session_id: &str) -> Result<Option<String>> {
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

pub async fn find_message(
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
