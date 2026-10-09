//! Serialized, daemon-owned inference/tool steps within one accepted user turn.
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use slop_core::provider::ProviderModelRef;

use crate::{
    chat::{
        ChatDelta, ChatOutcome, ChatRepository, ChatStatus, RoleKind, TurnWork, canceled, failed,
        finish_with_retry, interrupted_shutdown, provider_failure,
    },
    providers::{ProviderClient, Usage, UsageSource, inference::*},
    tools::{ToolOutcome, ToolService},
};

#[derive(Debug, Clone)]
pub struct RequestIntent {
    pub id: String,
    pub message_id: String,
    pub ordinal: u32,
    pub requested_model: String,
    pub requested_settings: GenerationSettings,
    pub settings: GenerationSettings,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolIntent {
    pub id: String,
    pub request_id: String,
    pub result_message_id: String,
    pub call_id: String,
    pub provider_call_id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone)]
pub struct RequestCompletion {
    pub request_id: String,
    pub response: InferenceResponse,
    pub tools: Vec<ToolIntent>,
}

pub fn new_id() -> std::io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(std::io::Error::other)?;
    Ok(format!("{:032x}", u128::from_le_bytes(bytes)))
}

fn aggregate(previous: Usage, next: Usage, first: bool) -> Usage {
    if first {
        return next;
    }
    let sum = |a: Option<u64>, b: Option<u64>| a.zip(b).and_then(|(a, b)| a.checked_add(b));
    let input = sum(previous.input_tokens, next.input_tokens);
    let output = sum(previous.output_tokens, next.output_tokens);
    let total = sum(previous.total_tokens, next.total_tokens);
    let reported = previous.total_source == Some(UsageSource::Reported)
        && next.total_source == Some(UsageSource::Reported);
    Usage::from_reported(input, output, if reported { total } else { None })
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

fn snapshot(
    request: &InferenceRequest,
    visible: &Mutex<BTreeMap<usize, String>>,
) -> InferenceMessage {
    let blocks = visible
        .lock()
        .map(|v| {
            v.iter()
                .map(|(index, text)| ContentBlock {
                    id: format!("{}:block:{index}", request.request_id),
                    content: BlockContent::Text { text: text.clone() },
                })
                .collect()
        })
        .unwrap_or_default();
    InferenceMessage {
        role: MessageRole::Assistant,
        blocks,
        continuation: None,
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute<R: ChatRepository>(
    repository: Arc<R>,
    providers: std::collections::HashMap<String, Arc<dyn ProviderClient>>,
    deltas: tokio::sync::broadcast::Sender<ChatDelta>,
    work: TurnWork,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
    tools: Arc<ToolService>,
) -> bool {
    let mut total = Usage::default();
    if *shutdown.borrow() {
        return stop(
            repository.as_ref(),
            &work.turn_id,
            interrupted_shutdown(),
            total,
        )
        .await;
    }
    match repository.cancellation_requested(&work.turn_id).await {
        Ok(true) => return stop(repository.as_ref(), &work.turn_id, canceled(), total).await,
        Ok(false) => {}
        Err(_) => return false,
    }
    let model = match work.requested_model.parse::<ProviderModelRef>() {
        Ok(model) => model,
        _ => {
            return stop(
                repository.as_ref(),
                &work.turn_id,
                failed("unsupported_model", "The requested model is not supported."),
                total,
            )
            .await;
        }
    };
    let Some(provider) = providers.get(model.provider().as_str()).cloned() else {
        return stop(
            repository.as_ref(),
            &work.turn_id,
            failed(
                "provider_auth_required",
                "Provider credentials are unavailable.",
            ),
            total,
        )
        .await;
    };
    let mut messages = if work.history.is_empty() {
        work.messages
            .iter()
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
                    message.text.clone(),
                )
            })
            .collect()
    } else {
        work.history.clone()
    };
    let settings = work.settings.clone();
    let definitions: Vec<ToolDefinition> = work
        .execution
        .as_ref()
        .map(|policy| {
            crate::tools::definitions()
                .into_iter()
                .filter(|d| policy.allowed_tools.contains(&d.name))
                .collect()
        })
        .unwrap_or_default();
    let max_requests = work
        .execution
        .as_ref()
        .map_or(1, |policy| policy.max_model_requests);
    let max_tools = work
        .execution
        .as_ref()
        .map_or(0, |policy| policy.max_tool_calls);
    let mut tool_count = 0;
    let mut cancel_tick = tokio::time::interval(Duration::from_millis(100));
    cancel_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    for ordinal in 1..=max_requests {
        if *shutdown.borrow() {
            return stop(
                repository.as_ref(),
                &work.turn_id,
                interrupted_shutdown(),
                total,
            )
            .await;
        }
        match repository.cancellation_requested(&work.turn_id).await {
            Ok(true) => return stop(repository.as_ref(), &work.turn_id, canceled(), total).await,
            Ok(false) => {}
            Err(_) => return false,
        }
        let request_id = match new_id() {
            Ok(id) => id,
            Err(_) => {
                return stop(
                    repository.as_ref(),
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
        let request = InferenceRequest {
            request_id: request_id.clone(),
            session_id: work.session_id.clone(),
            model: model.model().to_owned(),
            settings: settings.clone(),
            messages: messages.clone(),
            tools: definitions.clone(),
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
            return stop(repository.as_ref(), &work.turn_id, outcome, total).await;
        }
        let intent = RequestIntent {
            id: request_id.clone(),
            message_id: message_id.clone(),
            ordinal,
            requested_model: work.requested_model.clone(),
            requested_settings: work.requested_settings.clone(),
            settings: settings.clone(),
        };
        if repository
            .begin_request(&work.turn_id, intent)
            .await
            .is_err()
        {
            return false;
        }
        let visible = Arc::new(Mutex::new(BTreeMap::<usize, String>::new()));
        let stream_visible = Arc::clone(&visible);
        let stream_deltas = deltas.clone();
        let turn_id = work.turn_id.clone();
        let session_id = work.session_id.clone();
        let delta_request = request_id.clone();
        let delta_message = message_id.clone();
        let mut chunk_index = 0u64;
        let mut on_event = move |event: ProviderEvent| {
            let (index, text, kind) = match event {
                ProviderEvent::TextDelta { block_index, text } => {
                    if let Ok(mut v) = stream_visible.lock() {
                        v.entry(block_index).or_default().push_str(&text);
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
                    session_id: session_id.clone(),
                    turn_id: turn_id.clone(),
                    text: text[offset..end].to_owned(),
                    message_id: Some(delta_message.clone()),
                    block_id: Some(format!("{delta_request}:block:{index}")),
                    request_id: Some(delta_request.clone()),
                    invocation_id: None,
                    stream_id: format!("{delta_request}:{index}:{kind}"),
                    chunk_index,
                    kind: kind.into(),
                });
                chunk_index = chunk_index.saturating_add(1);
                offset = end;
            }
        };
        let mut inference = Box::pin(provider.infer(&request, &mut on_event));
        let mut checkpoint = tokio::time::interval(Duration::from_millis(250));
        checkpoint.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        checkpoint.tick().await;
        let mut last_snapshot = String::new();
        let result = loop {
            tokio::select! {
                result=&mut inference=>break result,
                _=shutdown.changed()=>{drop(inference);let _=repository.fail_request(&work.turn_id,&request_id,"daemon_shutdown").await;return stop(repository.as_ref(),&work.turn_id,interrupted_shutdown(),total).await;},
                _=cancel_tick.tick()=>match repository.cancellation_requested(&work.turn_id).await{
                    Ok(true)=>{drop(inference);let _=repository.fail_request(&work.turn_id,&request_id,"cancelled").await;return stop(repository.as_ref(),&work.turn_id,canceled(),total).await;},
                    Ok(false)=>{},Err(_)=>{drop(inference);return false;}
                },
                _=checkpoint.tick()=>{
                    let partial=snapshot(&request,&visible);let text=partial.visible_text();
                    if text!=last_snapshot && !text.is_empty(){
                        if repository.checkpoint_request(&work.turn_id,&request_id,partial).await.is_err(){
                            drop(inference);let cancelled=repository.cancellation_requested(&work.turn_id).await.unwrap_or(false);
                            let _=repository.fail_request(&work.turn_id,&request_id,if cancelled{"cancelled"}else{"storage_unavailable"}).await;
                            return stop(repository.as_ref(),&work.turn_id,if cancelled{canceled()}else{failed("storage_unavailable","A visible checkpoint could not be persisted.")},total).await;
                        }
                        last_snapshot=text;
                    }
                },
            }
        };
        drop(inference);
        let response = match result {
            Ok(response) => response,
            Err(error) => {
                let outcome = provider_failure(error);
                let _ = repository
                    .fail_request(
                        &work.turn_id,
                        &request_id,
                        outcome.error_code.as_deref().unwrap_or("provider_failed"),
                    )
                    .await;
                return stop(repository.as_ref(), &work.turn_id, outcome, total).await;
            }
        };
        total = aggregate(total, response.usage, ordinal == 1);
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
                return stop(repository.as_ref(), &work.turn_id, canceled(), total).await;
            }
            return false;
        }
        let finish = response.finish_reason.clone();
        messages.push(response.message.clone());
        if finish != FinishReason::ToolCalls {
            return stop(
                repository.as_ref(),
                &work.turn_id,
                ChatOutcome {
                    text: response.message.visible_text(),
                    resolved_model: response.resolved_model,
                    usage: total,
                    status: if finish == FinishReason::OutputLimit {
                        ChatStatus::Incomplete
                    } else {
                        ChatStatus::Completed
                    },
                    error_code: if finish == FinishReason::OutputLimit {
                        Some("provider_incomplete".into())
                    } else {
                        None
                    },
                    error_message: if finish == FinishReason::OutputLimit {
                        Some("The provider reached an output limit.".into())
                    } else {
                        None
                    },
                },
                total,
            )
            .await;
        }
        let exhausted = tool_count + invocations.len() as u32 > max_tools;
        for invocation in invocations {
            tool_count += 1;
            let policy = work.execution.clone();
            let outcome = if exhausted {
                ToolOutcome::failed("tool_budget_exhausted", false)
            } else if policy
                .as_ref()
                .is_none_or(|p| !p.allowed_tools.contains(&invocation.name))
            {
                ToolOutcome::failed("tool_not_allowed", false)
            } else if !crate::tools::validate_arguments(&invocation.name, &invocation.arguments) {
                ToolOutcome::failed("invalid_arguments", false)
            } else {
                if *shutdown.borrow() {
                    return stop(
                        repository.as_ref(),
                        &work.turn_id,
                        interrupted_shutdown(),
                        total,
                    )
                    .await;
                }
                if repository
                    .cancellation_requested(&work.turn_id)
                    .await
                    .unwrap_or(true)
                {
                    return stop(repository.as_ref(), &work.turn_id, canceled(), total).await;
                }
                if repository
                    .start_tool(&work.turn_id, &invocation)
                    .await
                    .is_err()
                {
                    if repository
                        .cancellation_requested(&work.turn_id)
                        .await
                        .unwrap_or(false)
                    {
                        return stop(repository.as_ref(), &work.turn_id, canceled(), total).await;
                    }
                    return false;
                }
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
                    policy.expect("checked policy"),
                    &invocation.name,
                    invocation.arguments.clone(),
                    cancel_rx,
                    &mut on_output,
                ));
                loop {
                    tokio::select! {
                        outcome=&mut execution=>break outcome,
                        _=shutdown.changed()=>{cancel_tx.send_replace(true);break execution.await;},
                        _=cancel_tick.tick()=>if repository.cancellation_requested(&work.turn_id).await.unwrap_or(true){cancel_tx.send_replace(true);break execution.await;},
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
                repository.as_ref(),
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
        repository.as_ref(),
        &work.turn_id,
        failed(
            "model_request_budget_exhausted",
            "The turn reached its model-request budget.",
        ),
        total,
    )
    .await
}
