use std::time::Duration;

use slop_client::{DaemonClient, EventStream};
use slop_protocol::chat::EventFrame;

use crate::{Result, history::find_message, output::Output};

const MAX_RENDERED_TURN_BYTES: usize = 2 * 1024 * 1024;

pub async fn follow_turn(
    client: &DaemonClient,
    session_id: &str,
    turn_id: &str,
    mut cursor: u64,
    output: Output,
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
                    output,
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
            output.terminal(&turn, canonical.as_ref(), &shown);
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
    output: Output,
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
                output.delta(visible)?;
            }
            EventFrame::Delta { .. } | EventFrame::Heartbeat => (),
        }
        output.frame(&frame)?;
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

pub async fn follow_session(
    client: &DaemonClient,
    session_id: &str,
    mut cursor: u64,
    output: Output,
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
                    output,
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
                        output.canonical(&turn, message.as_ref(), &shown);
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

pub fn is_terminal(status: &str) -> bool {
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
    fn rendered_text_limit_preserves_utf8_boundaries() {
        assert_eq!(bounded_utf8_prefix_len("a☃b", 4), "a☃".len());
        assert_eq!(bounded_utf8_prefix_len("a☃b", 3), "a".len());
        assert_eq!(bounded_utf8_prefix_len("☃", 2), 0);
    }
}
