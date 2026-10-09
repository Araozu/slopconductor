use std::collections::{HashMap, HashSet};
use std::time::Duration;

use slop_client::{DaemonClient, EventStream};
use slop_protocol::chat::EventFrame;

use crate::{Result, history::find_message, output::Output};

const MAX_RENDERED_TURN_BYTES: usize = 2 * 1024 * 1024;

#[derive(Default)]
struct StreamDisplay {
    active_message: Option<String>,
    completed: HashSet<String>,
    chunks: HashMap<String, u64>,
    rendered: usize,
    shown: String,
    terminal_turn: Option<String>,
}

pub async fn follow_turn(
    client: &DaemonClient,
    session_id: &str,
    turn_id: &str,
    mut cursor: u64,
    output: Output,
) -> Result<()> {
    let mut attempts = 0_u32;
    let mut display = StreamDisplay::default();
    loop {
        match client.events(session_id, cursor, true).await {
            Ok(mut stream) => {
                match consume_stream(
                    &mut stream,
                    &mut cursor,
                    turn_id,
                    output,
                    client,
                    &mut display,
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
            output.terminal(&turn, canonical.as_ref(), &display.shown);
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
    output: Output,
    client: &DaemonClient,
    display: &mut StreamDisplay,
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
                if (turn_id.is_empty() || event.turn_id.as_deref() == Some(turn_id))
                    && matches!(
                        event.kind.as_str(),
                        "assistant_message_completed" | "assistant_message_checkpointed"
                    )
                    && let Some(id) = &event.message_id
                    && !display.completed.contains(id)
                {
                    let message = client.message(id).await?;
                    if display.active_message.as_ref() != Some(id) {
                        display.shown.clear();
                        display.active_message = Some(id.clone());
                    }
                    output.message_snapshot(&message, &display.shown);
                    let keep = bounded_utf8_prefix_len(&message.text, MAX_RENDERED_TURN_BYTES);
                    display.shown.clear();
                    display.shown.push_str(&message.text[..keep]);
                    if message.status != "checkpoint" {
                        display.completed.insert(id.clone());
                    }
                }
                if (turn_id.is_empty() || event.turn_id.as_deref() == Some(turn_id))
                    && matches!(event.kind.as_str(), "tool_completed" | "tool_failed")
                    && let Some(id) = &event.invocation_id
                {
                    output.tool_snapshot(&client.tool_invocation(id).await?);
                }
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
                    display.terminal_turn = event.turn_id.clone();
                }
                *cursor = (*cursor).max(event.sequence);
            }
            EventFrame::Delta {
                turn_id: frame_turn,
                text,
                message_id,
                stream_id,
                chunk_index,
                kind,
                ..
            } if turn_id.is_empty() || frame_turn == turn_id => {
                if !stream_id.is_empty() {
                    if display
                        .chunks
                        .get(stream_id)
                        .is_some_and(|last| chunk_index <= last)
                    {
                        continue;
                    }
                    if display.chunks.len() >= 512 && !display.chunks.contains_key(stream_id) {
                        return Err("event stream exceeded its display bound".into());
                    }
                    display.chunks.insert(stream_id.clone(), *chunk_index);
                }
                if kind != "text"
                    || message_id
                        .as_ref()
                        .is_some_and(|id| display.completed.contains(id))
                {
                    output.frame(&frame)?;
                    continue;
                }
                if &display.active_message != message_id {
                    display.shown.clear();
                    display.active_message = message_id.clone();
                }
                let remaining = MAX_RENDERED_TURN_BYTES.saturating_sub(display.rendered);
                let visible_len = bounded_utf8_prefix_len(text, remaining);
                let visible = &text[..visible_len];
                display.shown.push_str(visible);
                display.rendered = display.rendered.saturating_add(visible.len());
                output.delta(visible)?;
            }
            EventFrame::Delta { .. } | EventFrame::Heartbeat => (),
        }
        output.frame(&frame)?;
        if display.terminal_turn.is_some() {
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
    let mut attempts = 0_u32;
    let mut display = StreamDisplay::default();
    loop {
        match client.events(session_id, cursor, true).await {
            Ok(mut stream) => {
                match consume_stream(&mut stream, &mut cursor, "", output, client, &mut display)
                    .await
                {
                    Ok(true) => return Ok(()),
                    Ok(false) if display.terminal_turn.is_some() => {
                        let turn_id = display.terminal_turn.take().expect("checked terminal turn");
                        let turn = client.turn(&turn_id).await?;
                        let message = if let Some(id) = &turn.assistant_message_id {
                            find_message(client, session_id, id).await?
                        } else {
                            None
                        };
                        output.canonical(&turn, message.as_ref(), &display.shown);
                        display.shown.clear();
                        display = StreamDisplay::default();
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
