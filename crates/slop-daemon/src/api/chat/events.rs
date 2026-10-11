use super::*;

pub(super) async fn events(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Query(query): Query<EventQuery>,
) -> Response {
    let after = query.after.unwrap_or(0);
    let session = match state.store.session(&session_id).await {
        Ok(session) => session,
        Err(error) => return store_error(error),
    };
    if after > session.last_event_sequence {
        return error(
            StatusCode::BAD_REQUEST,
            "invalid_cursor",
            "The event cursor is ahead of the durable session history.",
        );
    }
    let subscriber = match state.event_subscribers.clone().try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            return error(
                StatusCode::TOO_MANY_REQUESTS,
                "subscriber_limit",
                "The event subscriber limit has been reached.",
            );
        }
    };
    // Subscribe before the first durable catch-up read so a concurrent commit
    // is observed either in that read or by the wake-up which follows it.
    let receiver = state.runtime.subscribe();
    let feed = EventFeed {
        state,
        session_id,
        cursor: after,
        follow: query.follow.unwrap_or(false),
        receiver,
        pending: VecDeque::new(),
        heartbeat_at: tokio::time::Instant::now() + Duration::from_secs(5),
        _subscriber: subscriber,
    };
    let body = Body::from_stream(stream::unfold(feed, |mut feed| async move {
        let frame = feed.next_frame().await?;
        let bytes = serde_json::to_vec(&frame).ok()?;
        Some((
            Ok::<Bytes, Infallible>(Bytes::from([bytes, vec![b'\n']].concat())),
            feed,
        ))
    }));
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "application/x-ndjson")
        .body(body)
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

struct EventFeed {
    state: Arc<AppState>,
    session_id: String,
    cursor: u64,
    follow: bool,
    receiver: tokio::sync::broadcast::Receiver<slop_runtime::chat::ChatDelta>,
    pending: VecDeque<EventResponse>,
    heartbeat_at: tokio::time::Instant,
    _subscriber: tokio::sync::OwnedSemaphorePermit,
}

impl EventFeed {
    async fn next_frame(&mut self) -> Option<EventFrame> {
        loop {
            if let Some(event) = self.pending.pop_front() {
                self.cursor = event.sequence;
                return Some(EventFrame::Durable { event });
            }
            match self
                .state
                .store
                .events(&self.session_id, self.cursor, 100)
                .await
            {
                Ok(page) if !page.items.is_empty() => {
                    self.pending.extend(page.items);
                    continue;
                }
                Ok(_) if !self.follow => return None,
                Err(_) => return None,
                Ok(_) => {}
            }
            if !self.follow
                || !self
                    .state
                    .accepting
                    .load(std::sync::atomic::Ordering::Acquire)
            {
                return None;
            }
            tokio::select! {
                delta = self.receiver.recv() => match delta {
                    Ok(delta) if delta.session_id == self.session_id => return Some(EventFrame::Delta {
                        session_id: delta.session_id,
                        turn_id: delta.turn_id,
                        text: delta.text,
                        message_id: delta.message_id,
                        block_id: delta.block_id,
                        request_id: delta.request_id,
                        invocation_id: delta.invocation_id,
                        stream_id: delta.stream_id,
                        chunk_index: delta.chunk_index,
                        kind: delta.kind,
                    }),
                    Ok(_) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) | Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                },
                _ = tokio::time::sleep_until(self.heartbeat_at) => {
                    self.heartbeat_at = tokio::time::Instant::now() + Duration::from_secs(5);
                    return Some(EventFrame::Heartbeat);
                },
                _ = tokio::time::sleep(Duration::from_millis(100)) => {},
            }
        }
    }
}
