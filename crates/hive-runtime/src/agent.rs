//! Serialized, daemon-owned inference/tool steps within one accepted user turn.
pub mod context;
pub mod output_limit;
mod request_completion;
mod request_intent;
mod tool_intent;

pub use request_completion::RequestCompletion;
pub use request_intent::RequestIntent;
pub use tool_intent::ToolIntent;

use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, Mutex},
    time::Duration,
};

use hive_core::provider::ProviderModelRef;

use crate::{
    chat::{
        ChatDelta, ChatOutcome, ChatRepository, ChatStatus, RoleKind, TurnWork, canceled, failed,
        finish_with_retry, interrupted_shutdown, provider_failure,
    },
    providers::{ProviderClient, Usage, inference::*},
    tools::{ToolOutcome, ToolService},
};

pub fn new_id() -> std::io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(std::io::Error::other)?;
    Ok(format!("{:032x}", u128::from_le_bytes(bytes)))
}

async fn stop<R: ChatRepository>(
    repository: &R,
    turn: &str,
    mut outcome: ChatOutcome,
    usage: Usage,
) -> bool {
    outcome.usage = usage;
    finish_with_retry(repository, turn, outcome).await
}

/// Replace historical image blocks with a visible omission notice when the
/// turn's model cannot accept image input (e.g. after a mid-conversation
/// model switch). New image messages to such models are rejected at
/// acceptance; this only rewrites the in-memory inference view, never storage.
fn omit_images_for_text_model(
    messages: Vec<InferenceMessage>,
    model: &str,
) -> Vec<InferenceMessage> {
    messages
        .into_iter()
        .map(|mut message| {
            for block in &mut message.blocks {
                if matches!(block.content, BlockContent::Image { .. }) {
                    block.content = BlockContent::Text {
                        text: format!("[image omitted: {model} does not accept image input]"),
                    };
                }
            }
            message
        })
        .collect()
}

/// Load and verify image bytes for every image block in the request.
/// Fails the turn with a clear error instead of silently dropping images.
async fn resolve_images(
    tools: &ToolService,
    messages: &[InferenceMessage],
) -> Result<HashMap<String, ResolvedImage>, (&'static str, &'static str)> {
    let mut images = HashMap::new();
    for message in messages {
        for block in &message.blocks {
            if let BlockContent::Image {
                artifact_id,
                media_type,
            } = &block.content
            {
                if images.contains_key(artifact_id) {
                    continue;
                }
                let bytes = tools
                    .read_verified_artifact(artifact_id)
                    .await
                    .map_err(|_| {
                        (
                            "image_unavailable",
                            "An attached image is missing or corrupt; re-upload it and retry.",
                        )
                    })?;
                images.insert(
                    artifact_id.clone(),
                    ResolvedImage {
                        media_type: media_type.clone(),
                        bytes: Arc::from(bytes.into_boxed_slice()),
                    },
                );
            }
        }
    }
    Ok(images)
}

#[derive(Default)]
struct VisibleOutput {
    blocks: BTreeMap<usize, String>,
    bytes: usize,
}

fn snapshot(
    request_id: &str,
    visible: &Mutex<VisibleOutput>,
    checkpoint_bytes: usize,
) -> Option<(InferenceMessage, usize)> {
    let visible = visible.lock().ok()?;
    if visible.bytes == checkpoint_bytes {
        return None;
    }
    let blocks = visible
        .blocks
        .iter()
        .map(|(index, text)| ContentBlock {
            id: format!("{request_id}:block:{index}"),
            content: BlockContent::Text { text: text.clone() },
        })
        .collect();
    Some((
        InferenceMessage {
            role: MessageRole::Assistant,
            blocks,
            continuation: None,
        },
        visible.bytes,
    ))
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute<R: ChatRepository>(
    repository: &R,
    provider: Option<&dyn ProviderClient>,
    deltas: &tokio::sync::broadcast::Sender<ChatDelta>,
    mut work: TurnWork,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
    tools: &ToolService,
) -> bool {
    let mut total = work.initial_usage;
    if *shutdown.borrow() {
        return stop(repository, &work.turn_id, interrupted_shutdown(), total).await;
    }
    match repository.cancellation_requested(&work.turn_id).await {
        Ok(true) => return stop(repository, &work.turn_id, canceled(), total).await,
        Ok(false) => {}
        Err(_) => return false,
    }
    let model = match work.requested_model.parse::<ProviderModelRef>() {
        Ok(model) => model,
        _ => {
            return stop(
                repository,
                &work.turn_id,
                failed("unsupported_model", "The requested model is not supported."),
                total,
            )
            .await;
        }
    };
    let Some(provider) = provider else {
        return stop(
            repository,
            &work.turn_id,
            failed(
                "provider_auth_required",
                "Provider credentials are unavailable.",
            ),
            total,
        )
        .await;
    };
    match repository.prepare_workspace(&work.turn_id, shutdown).await {
        Ok(Some((code, message))) => {
            return stop(repository, &work.turn_id, failed(code, message), total).await;
        }
        Ok(None) => {}
        Err(_) => return false,
    }
    let mut messages = if work.history.is_empty() {
        std::mem::take(&mut work.messages)
            .into_iter()
            .enumerate()
            .map(|(index, message)| {
                InferenceMessage::text(
                    match message.role {
                        RoleKind::System => MessageRole::System,
                        RoleKind::Developer => MessageRole::Developer,
                        RoleKind::User => MessageRole::User,
                        RoleKind::Assistant => MessageRole::Assistant,
                    },
                    format!("{}:context:{index}", work.turn_id),
                    message.text,
                )
            })
            .collect()
    } else {
        work.messages.clear();
        std::mem::take(&mut work.history)
    };
    let settings = work.settings.clone();
    let mut definitions: Vec<ToolDefinition> = work
        .execution
        .as_ref()
        .map(|policy| {
            crate::tools::definitions()
                .into_iter()
                .filter(|d| policy.allowed_tools.contains(&d.name))
                .collect()
        })
        .unwrap_or_default();
    if let Some(policy) = &work.orchestration {
        definitions.extend(crate::orchestration::definitions(
            policy,
            &work.workspace_id,
        ));
    }
    let max_requests = work.max_model_requests.max(1);
    let max_tools = work.execution.as_ref().map_or(
        if work.orchestration.is_some() {
            u32::MAX
        } else {
            0
        },
        |policy| policy.max_tool_calls,
    );
    let mut tool_count = work.tool_calls_used;
    let mut ordinal = work.next_request_ordinal.max(1);
    let mut output_limits_seen = work.output_limits_seen;
    let mut context_usage = work.context_usage;
    let Some(mut context_budget) = context::token_budget(
        work.context_token_budget,
        &settings,
        &provider.capabilities(model.model()),
    ) else {
        return stop(
            repository,
            &work.turn_id,
            failed(
                "unsupported_capability",
                "The model's full context window is unknown.",
            ),
            total,
        )
        .await;
    };
    let mut compactions_seen = work.compactions_seen;
    let mut force_compaction = work.context_overflow;
    let mut context_frontier = Some(work.context_through_ordinal);
    let mut usage_started = work.next_request_ordinal > 1;
    let mut transient_retries = 0u32;
    let mut cancel_tick = tokio::time::interval(Duration::from_millis(100));
    cancel_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    while ordinal <= max_requests {
        if *shutdown.borrow() {
            return stop(repository, &work.turn_id, interrupted_shutdown(), total).await;
        }
        let control = match repository.turn_control(&work.turn_id).await {
            Ok(control) => control,
            Err(_) => return false,
        };
        if control.cancellation_requested {
            return stop(repository, &work.turn_id, canceled(), total).await;
        }
        if control.pause_requested {
            return stop(
                repository,
                &work.turn_id,
                steering_yield("pause_turn", total),
                total,
            )
            .await;
        }
        if control.waiting_requested {
            return stop(
                repository,
                &work.turn_id,
                steering_yield("awaiting_children", total),
                total,
            )
            .await;
        }
        if control.steering_pending {
            return stop(
                repository,
                &work.turn_id,
                steering_yield("steering_resume", total),
                total,
            )
            .await;
        }
        let request_id = match new_id() {
            Ok(id) => id,
            Err(_) => {
                return stop(
                    repository,
                    &work.turn_id,
                    failed(
                        "entropy_unavailable",
                        "Request identity could not be allocated.",
                    ),
                    total,
                )
                .await;
            }
        };
        let message_id = match new_id() {
            Ok(id) => id,
            Err(_) => return false,
        };
        let compact = force_compaction
            || context::needs_compaction(
                &messages,
                &definitions,
                context_usage,
                context_budget,
                settings.max_output_tokens,
            );
        let mut compaction_plan = if compact && compactions_seen < context::MAX_COMPACTIONS_PER_TURN
        {
            context::plan(
                &messages,
                context_budget,
                work.context_overflow,
                force_compaction
                    || context_usage.is_some_and(|usage| {
                        usage.tokens()
                            > context::input_bytes(&messages, &definitions)
                                .div_ceil(3)
                                .saturating_mul(2)
                    }),
            )
        } else {
            None
        };
        if force_compaction && compaction_plan.is_none() {
            return stop(repository, &work.turn_id,
                failed("context_limit", "The context cannot be reduced further within the compaction budget. Shorten the latest input or increase context_token_budget."), total).await;
        }
        let is_compaction = compaction_plan.is_some();
        if is_compaction && ordinal == max_requests {
            return stop(repository, &work.turn_id,
                failed("model_budget_exhausted", "Context compaction needs a summary request and a continuation, but this turn has no remaining model-request budget."), total).await;
        }
        let request_messages = compaction_plan.as_mut().map_or_else(
            || std::mem::take(&mut messages),
            |plan| std::mem::take(&mut plan.prompt),
        );
        let request_tools = if is_compaction {
            Vec::new()
        } else {
            std::mem::take(&mut definitions)
        };
        let image_input = provider.capabilities(model.model()).image_input;
        let (request_messages, resolved_images) = if is_compaction {
            (request_messages, Default::default())
        } else if !image_input {
            (
                omit_images_for_text_model(request_messages, &work.requested_model),
                Default::default(),
            )
        } else {
            match resolve_images(tools, &request_messages).await {
                Ok(images) => (request_messages, images),
                Err((code, message)) => {
                    return stop(repository, &work.turn_id, failed(code, message), total).await;
                }
            }
        };
        let request = InferenceRequest {
            request_id: request_id.clone(),
            session_id: work.session_id.clone(),
            model: model.model().to_owned(),
            settings: if is_compaction {
                context::summary_settings(
                    provider.auth_mode() != crate::providers::AuthMode::ChatGptSubscription,
                )
            } else {
                settings.clone()
            },
            messages: request_messages,
            tools: request_tools,
            images: resolved_images,
        };
        if let Err(error) = request.validate(
            provider.descriptor(),
            &provider.capabilities(&request.model),
        ) {
            let outcome = match error {
                crate::providers::ProviderError::UnsupportedCapability { capability }
                    if capability.starts_with("context_incompatible") =>
                {
                    failed(
                        "context_incompatible",
                        "The selected model cannot replay required continuation from this history.",
                    )
                }
                crate::providers::ProviderError::UnsupportedCapability { .. } => failed(
                    "unsupported_capability",
                    "The selected model does not support a requested feature or setting.",
                ),
                crate::providers::ProviderError::LimitExceeded { .. } => {
                    failed("context_limit", "Turn context exceeds the supported bound.")
                }
                error => provider_failure(error),
            };
            return stop(repository, &work.turn_id, outcome, total).await;
        }
        let request_input_bytes = context::input_bytes(&request.messages, &request.tools);
        let intent = RequestIntent {
            id: request_id.clone(),
            message_id: message_id.clone(),
            ordinal,
            requested_model: work.requested_model.clone(),
            requested_settings: work.requested_settings.clone(),
            settings: request.settings.clone(),
            context: context::RequestContext {
                input_bytes: request_input_bytes,
                compaction: is_compaction,
            },
        };
        let started = match repository.begin_request(&work.turn_id, intent).await {
            Ok(started) => started,
            Err(_) => return false,
        };
        if !started {
            let control = match repository.turn_control(&work.turn_id).await {
                Ok(control) => control,
                Err(_) => return false,
            };
            if control.cancellation_requested {
                return stop(repository, &work.turn_id, canceled(), total).await;
            }
            if control.waiting_requested {
                return stop(
                    repository,
                    &work.turn_id,
                    steering_yield("awaiting_children", total),
                    total,
                )
                .await;
            }
            if control.budget_exhausted {
                return stop(
                    repository,
                    &work.turn_id,
                    failed(
                        "operation_budget_exhausted",
                        "The task or batch reached its aggregate operation budget.",
                    ),
                    total,
                )
                .await;
            }
            let code = if control.pause_requested {
                "pause_turn"
            } else {
                "steering_resume"
            };
            return stop(
                repository,
                &work.turn_id,
                steering_yield(code, total),
                total,
            )
            .await;
        }
        ordinal = ordinal.saturating_add(1);
        if is_compaction {
            compactions_seen = compactions_seen.saturating_add(1);
        }
        // Inference borrows the callback. Checkpoint polling shares its output
        // within this supervisor; neither needs independently owned Arc state.
        let visible = Mutex::new(VisibleOutput::default());
        let stream_visible = &visible;
        let stream_deltas = deltas;
        let turn_id = work.turn_id.as_str();
        let session_id = work.session_id.as_str();
        let delta_request = request_id.as_str();
        let delta_message = message_id.as_str();
        let mut chunk_index = 0u64;
        let mut on_event = move |event: ProviderEvent| {
            if is_compaction {
                return;
            }
            let (index, text, kind) = match event {
                ProviderEvent::TextDelta { block_index, text } => {
                    if let Ok(mut v) = stream_visible.lock() {
                        v.blocks.entry(block_index).or_default().push_str(&text);
                        v.bytes += text.len();
                    }
                    (block_index, text, "text")
                }
                ProviderEvent::ToolArgumentsDelta {
                    block_index,
                    fragment,
                } => (block_index, fragment, "tool_arguments"),
                ProviderEvent::UsageUpdated { .. } => return,
            };
            let mut offset = 0;
            while offset < text.len() {
                let mut end = (offset + 16 * 1024).min(text.len());
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                let _ = stream_deltas.send(ChatDelta {
                    session_id: session_id.to_owned(),
                    turn_id: turn_id.to_owned(),
                    text: text[offset..end].to_owned(),
                    message_id: Some(delta_message.to_owned()),
                    block_id: Some(format!("{delta_request}:block:{index}")),
                    request_id: Some(delta_request.to_owned()),
                    invocation_id: None,
                    stream_id: format!("{delta_request}:{index}:{kind}"),
                    chunk_index,
                    kind: kind.into(),
                });
                chunk_index = chunk_index.saturating_add(1);
                offset = end;
            }
        };
        let mut inference = provider.infer(&request, &mut on_event);
        let mut checkpoint = tokio::time::interval(Duration::from_millis(250));
        checkpoint.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        checkpoint.tick().await;
        let mut checkpoint_bytes = 0;
        let result = loop {
            tokio::select! {
                result=&mut inference=>break result,
                _=shutdown.changed()=>{drop(inference);let _=repository.fail_request(&work.turn_id,&request_id,"daemon_shutdown").await;return stop(repository,&work.turn_id,interrupted_shutdown(),total).await;},
                _=cancel_tick.tick()=>match repository.turn_control(&work.turn_id).await{
                    Ok(control) if control.cancellation_requested=>{
                        drop(inference);
                        let _=repository.fail_request(&work.turn_id,&request_id,"cancelled").await;
                        return stop(repository,&work.turn_id,canceled(),total).await;
                    },
                    Ok(control) if control.immediate_requested=>{
                        drop(inference);
                        let _=repository.fail_request(&work.turn_id,&request_id,"steering_interrupted").await;
                        return stop(repository,&work.turn_id,steering_yield("steering_resume_unknown",Usage::default()),Usage::default()).await;
                    },
                    Ok(_)=>{},Err(_)=>{drop(inference);return false;}
                },
                _=checkpoint.tick()=>{
                    if let Some((partial, bytes)) = snapshot(&request_id, &visible, checkpoint_bytes) {
                        if repository.checkpoint_request(&work.turn_id,&request_id,partial).await.is_err(){
                            drop(inference);let cancelled=repository.cancellation_requested(&work.turn_id).await.unwrap_or(false);
                            let _=repository.fail_request(&work.turn_id,&request_id,if cancelled{"cancelled"}else{"storage_unavailable"}).await;
                            return stop(repository,&work.turn_id,if cancelled{canceled()}else{failed("storage_unavailable","A visible checkpoint could not be persisted.")},total).await;
                        }
                        checkpoint_bytes=bytes;
                    }
                },
            }
        };
        drop(inference);
        drop(visible);
        // Return ownership after the borrowed inference future finishes. Error
        // recovery, tool steps, and output continuations reuse these same buffers.
        if !is_compaction {
            messages = request.messages;
            definitions = request.tools;
        } else {
            drop(request.messages);
            drop(request.tools);
        }
        let mut response = match result {
            Ok(response) => response,
            Err(error) => {
                let context_error = matches!(
                    error,
                    crate::providers::ProviderError::ContextLengthExceeded
                );
                let retry_delay = match &error {
                    crate::providers::ProviderError::RateLimited { retry_after } => {
                        Some(retry_after.unwrap_or(Duration::from_millis(
                            500 * (1 << transient_retries.min(2)),
                        )))
                    }
                    crate::providers::ProviderError::UnexpectedStatus {
                        status: 500..=599, ..
                    } => Some(Duration::from_millis(500 * (1 << transient_retries.min(2)))),
                    crate::providers::ProviderError::Http(error)
                        if error.is_connect() || error.is_timeout() =>
                    {
                        Some(Duration::from_millis(500 * (1 << transient_retries.min(2))))
                    }
                    _ => None,
                };
                let outcome = provider_failure(error);
                let _ = repository
                    .fail_request(
                        &work.turn_id,
                        &request_id,
                        outcome.error_code.as_deref().unwrap_or("provider_failed"),
                    )
                    .await;
                if context_error
                    && ordinal <= max_requests
                    && compactions_seen < context::MAX_COMPACTIONS_PER_TURN
                {
                    context_budget = (context_budget / 2).max(8192);
                    context_usage = None;
                    force_compaction = true;
                    continue;
                }
                if let Some(delay) = retry_delay.filter(|d| *d <= Duration::from_secs(5))
                    && transient_retries < 2
                    && ordinal <= max_requests
                {
                    transient_retries += 1;
                    // A failed/timeout inference can have consumed tokens. Preserve
                    // unknown billing counters instead of inventing zero usage.
                    total = Usage::default();
                    usage_started = true;
                    let deadline = tokio::time::Instant::now() + delay;
                    loop {
                        tokio::select! {
                            _ = tokio::time::sleep_until(deadline) => break,
                            _ = shutdown.changed() => return stop(repository, &work.turn_id, interrupted_shutdown(), total).await,
                            _ = cancel_tick.tick() => match repository.turn_control(&work.turn_id).await {
                                Ok(control) if control.cancellation_requested => return stop(repository, &work.turn_id, canceled(), total).await,
                                Ok(control) if control.pause_requested || control.steering_pending => return stop(repository, &work.turn_id, steering_yield(if control.pause_requested { "pause_turn" } else { "steering_resume" }, total), total).await,
                                Ok(_) => {}, Err(_) => return false,
                            }
                        }
                    }
                    continue;
                }
                return stop(repository, &work.turn_id, outcome, total).await;
            }
        };
        // Only a completed tool proposal can launch a side effect. Keep visible
        // partial text, but never replay incomplete reasoning or tool arguments.
        if matches!(
            response.finish_reason,
            FinishReason::OutputLimit
                | FinishReason::ContextLimit
                | FinishReason::Incomplete
                | FinishReason::ContentFilter
        ) {
            response.message.continuation = None;
            response.message.blocks.retain(|block| {
                matches!(
                    block.content,
                    BlockContent::Text { .. } | BlockContent::Refusal { .. }
                )
            });
            if response.message.blocks.is_empty() {
                response.message = InferenceMessage::text(
                    MessageRole::Assistant,
                    format!("{request_id}:block:0"),
                    String::new(),
                );
            }
        }
        total = if usage_started {
            total.accumulate(response.usage)
        } else {
            response.usage
        };
        usage_started = true;
        if let Some(plan) = compaction_plan {
            let summary = response.message.visible_text();
            let usable = response.finish_reason == FinishReason::Stop
                && !summary.trim().is_empty()
                && !response
                    .message
                    .blocks
                    .iter()
                    .any(|b| !matches!(b.content, BlockContent::Text { .. }));
            let compacted = usable.then(|| plan.finish(&request_id, summary));
            let compacted =
                compacted.filter(|next| context::is_reduced(&messages, next, context_usage));
            if repository
                .complete_request(
                    &work.turn_id,
                    RequestCompletion {
                        request_id: request_id.clone(),
                        response,
                        tools: Vec::new(),
                        compaction: compacted
                            .clone()
                            .map(|messages| context::ContextCheckpoint {
                                messages,
                                through_ordinal: context_frontier,
                            }),
                    },
                )
                .await
                .is_err()
            {
                return false;
            }
            let Some(compacted) = compacted else {
                return stop(repository, &work.turn_id,
                    failed("context_compaction_failed", "The provider did not produce a complete, smaller context summary. The original history was preserved."), total).await;
            };
            messages = compacted;
            context_usage = None;
            force_compaction = false;
            if work.context_overflow {
                return stop(
                    repository,
                    &work.turn_id,
                    steering_yield("context_compaction_resume", total),
                    total,
                )
                .await;
            }
            continue;
        }
        context_usage = response.usage.input_tokens.map(|input_tokens| {
            context::ContextUsage::after_response(
                input_tokens,
                response.usage.output_tokens,
                request_input_bytes,
                &response.message,
            )
        });
        context_frontier = None;
        transient_retries = 0;
        // A successful normal request starts a new context-growth cycle. The
        // repair allowance bounds consecutive maintenance attempts, not the
        // number of compactions a long-running agent can perform in its life.
        if response.finish_reason != FinishReason::ContextLimit {
            compactions_seen = 0;
        }
        let mut invocations = Vec::new();
        for block in &response.message.blocks {
            if let BlockContent::ToolCall {
                call_id,
                provider_call_id,
                name,
                arguments,
            } = &block.content
            {
                let (Ok(id), Ok(result_message_id)) = (new_id(), new_id()) else {
                    return false;
                };
                invocations.push(ToolIntent {
                    id,
                    request_id: request_id.clone(),
                    result_message_id,
                    call_id: call_id.clone(),
                    provider_call_id: provider_call_id.clone(),
                    name: name.clone(),
                    arguments: arguments.clone(),
                });
            }
        }
        if repository
            .complete_request(
                &work.turn_id,
                RequestCompletion {
                    request_id: request_id.clone(),
                    response: response.clone(),
                    tools: invocations.clone(),
                    compaction: None,
                },
            )
            .await
            .is_err()
        {
            if repository
                .cancellation_requested(&work.turn_id)
                .await
                .unwrap_or(false)
            {
                return stop(repository, &work.turn_id, canceled(), total).await;
            }
            return false;
        }
        let finish = response.finish_reason.clone();
        let text = response.message.visible_text();
        let no_progress = output_limits_seen > 0
            && messages
                .iter()
                .rev()
                .find(|message| message.role == MessageRole::Assistant)
                .is_some_and(|previous| previous.visible_text().trim() == text.trim());
        let refused = response
            .message
            .blocks
            .iter()
            .any(|block| matches!(block.content, BlockContent::Refusal { .. }));
        messages.push(response.message);
        if finish == FinishReason::ContextLimit && ordinal <= max_requests {
            messages.push(output_limit::context_continuation_prompt(&request_id));
            context_budget = (context_budget / 2).max(8192);
            force_compaction = true;
            continue;
        }
        if finish == FinishReason::OutputLimit {
            output_limits_seen = output_limits_seen.saturating_add(1);
            if !no_progress
                && !refused
                && output_limits_seen <= output_limit::MAX_CONTINUATIONS
                && ordinal <= max_requests
            {
                messages.push(output_limit::continuation_prompt(&request_id));
                continue;
            }
        }
        if finish != FinishReason::ToolCalls {
            let incomplete = matches!(
                finish,
                FinishReason::OutputLimit
                    | FinishReason::ContextLimit
                    | FinishReason::Incomplete
                    | FinishReason::ContentFilter
            );
            let error_message = match finish {
                FinishReason::OutputLimit if no_progress => Some(
                    "The provider reached its output-token limit again without progress. Increase the output limit or request smaller steps.",
                ),
                FinishReason::OutputLimit => Some(
                    "The provider reached its output-token limit and automatic continuation has no remaining budget. Increase the output limit or request smaller steps.",
                ),
                FinishReason::ContextLimit => Some(
                    "The provider context window was reached and this turn has no remaining compaction budget.",
                ),
                FinishReason::ContentFilter => {
                    Some("The provider stopped the response with a content filter.")
                }
                FinishReason::Incomplete => Some(
                    "The provider returned an incomplete response without a recognized output-token limit.",
                ),
                _ => None,
            };
            return stop(
                repository,
                &work.turn_id,
                ChatOutcome {
                    text,
                    resolved_model: response.resolved_model,
                    usage: total,
                    status: if incomplete {
                        ChatStatus::Incomplete
                    } else {
                        ChatStatus::Completed
                    },
                    error_code: if incomplete {
                        Some("provider_incomplete".into())
                    } else {
                        None
                    },
                    error_message: error_message.map(str::to_owned),
                },
                total,
            )
            .await;
        }
        let exhausted = tool_count + invocations.len() as u32 > max_tools;
        for invocation in invocations {
            tool_count += 1;
            let policy = work.execution.as_ref();
            let orchestration = crate::orchestration::NAMES.contains(&invocation.name.as_str());
            let outcome = if exhausted {
                ToolOutcome::failed("tool_budget_exhausted", false)
            } else if if orchestration {
                work.orchestration.is_none()
            } else {
                policy.is_none_or(|p| !p.allowed_tools.contains(&invocation.name))
            } {
                ToolOutcome::failed("tool_not_allowed", false)
            } else if !(if orchestration {
                crate::orchestration::validate_arguments(&invocation.name, &invocation.arguments)
            } else {
                crate::tools::validate_arguments(&invocation.name, &invocation.arguments)
            }) {
                ToolOutcome::failed("invalid_arguments", false)
            } else {
                if *shutdown.borrow() {
                    return stop(repository, &work.turn_id, interrupted_shutdown(), total).await;
                }
                if repository
                    .cancellation_requested(&work.turn_id)
                    .await
                    .unwrap_or(true)
                {
                    return stop(repository, &work.turn_id, canceled(), total).await;
                }
                match repository.start_tool(&work.turn_id, &invocation).await {
                    Ok(true) => {}
                    Ok(false) => {
                        let control = match repository.turn_control(&work.turn_id).await {
                            Ok(control) => control,
                            Err(_) => return false,
                        };
                        if control.cancellation_requested {
                            return stop(repository, &work.turn_id, canceled(), total).await;
                        }
                        if control.waiting_requested {
                            return stop(
                                repository,
                                &work.turn_id,
                                steering_yield("awaiting_children", total),
                                total,
                            )
                            .await;
                        }
                        if control.budget_exhausted {
                            return stop(
                                repository,
                                &work.turn_id,
                                failed(
                                    "operation_budget_exhausted",
                                    "The task or batch reached its aggregate operation budget.",
                                ),
                                total,
                            )
                            .await;
                        }
                        let code = if control.pause_requested {
                            "pause_turn"
                        } else {
                            "steering_resume"
                        };
                        return stop(
                            repository,
                            &work.turn_id,
                            steering_yield(code, total),
                            total,
                        )
                        .await;
                    }
                    Err(_) => return false,
                }
                if orchestration {
                    match repository
                        .orchestration_tool(&work.turn_id, &invocation)
                        .await
                    {
                        Ok(crate::orchestration::OrchestrationResult::Completed(outcome)) => {
                            outcome
                        }
                        Ok(crate::orchestration::OrchestrationResult::Waiting) => {
                            return stop(
                                repository,
                                &work.turn_id,
                                steering_yield("awaiting_children", total),
                                total,
                            )
                            .await;
                        }
                        Err(_) => return false,
                    }
                } else {
                    let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
                    let session_id = work.session_id.clone();
                    let turn_id = work.turn_id.clone();
                    let id = invocation.id.clone();
                    let request_id = invocation.request_id.clone();
                    let result_message_id = invocation.result_message_id.clone();
                    let mut chunk_index = 0u64;
                    let mut on_output = |kind: &str, text: &str| {
                        let _ = deltas.send(ChatDelta {
                            session_id: session_id.clone(),
                            turn_id: turn_id.clone(),
                            text: text.into(),
                            message_id: Some(result_message_id.clone()),
                            block_id: None,
                            request_id: Some(request_id.clone()),
                            invocation_id: Some(id.clone()),
                            stream_id: format!("{id}:{kind}"),
                            chunk_index,
                            kind: kind.into(),
                        });
                        chunk_index = chunk_index.saturating_add(1);
                    };
                    let mut execution = Box::pin(tools.run(
                        policy.expect("checked policy").clone(),
                        &invocation.name,
                        invocation.arguments.clone(),
                        cancel_rx,
                        &mut on_output,
                    ));
                    loop {
                        tokio::select! {
                            outcome=&mut execution=>break outcome,
                            _=shutdown.changed()=>{cancel_tx.send_replace(true);break execution.await;},
                            _=cancel_tick.tick()=>match repository.turn_control(&work.turn_id).await{
                                Ok(control) if control.cancellation_requested || control.immediate_requested=>{cancel_tx.send_replace(true);break execution.await;},
                                Ok(_)=>{},Err(_)=>{cancel_tx.send_replace(true);break execution.await;}
                            },
                        }
                    }
                }
            };
            let mut persisted = false;
            for delay in [0, 25, 50, 100, 200] {
                if delay > 0 {
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                if repository
                    .finish_tool(&work.turn_id, &invocation, outcome.clone())
                    .await
                    .is_ok()
                {
                    persisted = true;
                    break;
                }
            }
            if !persisted {
                return false;
            }
            messages.push(InferenceMessage {
                role: MessageRole::Tool,
                blocks: vec![ContentBlock {
                    id: format!("{}:result", invocation.id),
                    content: BlockContent::ToolResult {
                        call_id: invocation.call_id,
                        provider_call_id: invocation.provider_call_id,
                        is_error: outcome.status != "completed",
                        output: outcome.output,
                        artifact_ids: outcome.artifacts.iter().map(|a| a.id.clone()).collect(),
                        effects_unknown: outcome.effects_unknown,
                    },
                }],
                continuation: None,
            });
        }
        if exhausted {
            return stop(
                repository,
                &work.turn_id,
                failed(
                    "tool_budget_exhausted",
                    "The turn reached its tool-call budget.",
                ),
                total,
            )
            .await;
        }
    }
    stop(
        repository,
        &work.turn_id,
        failed(
            "model_request_budget_exhausted",
            "The turn reached its model-request budget.",
        ),
        total,
    )
    .await
}

fn steering_yield(code: &str, usage: Usage) -> ChatOutcome {
    ChatOutcome {
        text: String::new(),
        resolved_model: None,
        usage,
        status: ChatStatus::Interrupted,
        error_code: Some(code.to_owned()),
        error_message: Some("The active turn yielded at an execution boundary.".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn historical_images_become_omission_text_for_text_only_models() {
        let messages = vec![InferenceMessage {
            role: MessageRole::User,
            blocks: vec![
                ContentBlock {
                    id: "text".into(),
                    content: BlockContent::Text {
                        text: "See this".into(),
                    },
                },
                ContentBlock {
                    id: "image".into(),
                    content: BlockContent::Image {
                        artifact_id: "ab".repeat(32),
                        media_type: "image/png".into(),
                    },
                },
            ],
            continuation: None,
        }];
        let omitted = omit_images_for_text_model(messages, "opencode-go/hy3");
        assert_eq!(omitted.len(), 1);
        assert!(
            !omitted[0]
                .blocks
                .iter()
                .any(|b| matches!(b.content, BlockContent::Image { .. }))
        );
        let visible = omitted[0].visible_text();
        assert!(visible.contains("See this"));
        assert!(visible.contains("[image omitted: opencode-go/hy3 does not accept image input]"));
        // Text-only messages pass through untouched.
        let plain = vec![InferenceMessage::text(
            MessageRole::User,
            "plain".into(),
            "hello".into(),
        )];
        let kept = omit_images_for_text_model(plain.clone(), "opencode-go/hy3");
        assert_eq!(kept, plain);
    }
}
