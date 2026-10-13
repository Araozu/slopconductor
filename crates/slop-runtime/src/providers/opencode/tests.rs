use super::super::OpencodeGoProvider;
use super::decode::{chat_outcome, incomplete, messages_outcome, parse_openai_usage, parse_usage};
use super::requests::{to_anthropic_parts, to_responses_input};
use super::stream::{merge_sse_usage, parse_sse_line, process_sse_body};

#[test]
fn anthropic_cached_input_counts_toward_context_without_double_counting_openai_cache() {
    let mut usage = Usage::default();
    merge_sse_usage(
        &mut usage,
        &serde_json::json!({"type":"message_start","message":{"usage":{
            "input_tokens":42,"cache_creation_input_tokens":1000,"cache_read_input_tokens":30000,"output_tokens":0
        }}}),
    );
    merge_sse_usage(
        &mut usage,
        &serde_json::json!({"usage":{"output_tokens":12}}),
    );
    assert_eq!(usage.input_tokens, Some(31042));
    assert_eq!(usage.total_tokens, Some(31054));
    let openai = parse_usage(
        &serde_json::json!({"prompt_tokens":30000,"prompt_tokens_details":{"cached_tokens":29000},"completion_tokens":10}),
    );
    assert_eq!(openai.input_tokens, Some(30000));
    let overflowing = parse_usage(
        &serde_json::json!({"input_tokens":u64::MAX,"cache_read_input_tokens":1,"output_tokens":2}),
    );
    assert_eq!(overflowing.input_tokens, None);
    assert_eq!(overflowing.total_tokens, None);
}
use super::*;

#[test]
fn upstream_status_errors_exclude_body_and_credentials() {
    let secret = "oc_sk_synthetic_secret_123";
    let client =
        OpencodeClient::new(OpencodeGoProvider::instance(), secret).expect("client builds");
    let err = client.status_error(401);
    for rendered in [err.to_string(), format!("{err:?}")] {
        assert!(!rendered.contains(secret), "{rendered}");
        assert!(rendered.contains("401"), "{rendered}");
    }
}

#[test]
fn transport_diagnostics_exclude_raw_error_urls() {
    let secret = "synthetic_query_secret";
    let url = reqwest::Url::parse(&format!("https://example.invalid/?token={secret}")).unwrap();
    let source = reqwest::Client::new()
        .get("invalid relative URL")
        .build()
        .unwrap_err()
        .with_url(url);
    let err = ProviderError::Http(source);
    for rendered in [err.to_string(), format!("{err:?}")] {
        assert!(!rendered.contains(secret), "{rendered}");
        assert!(!rendered.contains("example.invalid"), "{rendered}");
    }
}

#[test]
fn parses_live_chat_shape() {
    let body = serde_json::json!({
        "id": "chatcmpl-test",
        "choices": [{
            "index": 0,
            "finish_reason": "stop",
            "message": {"role": "assistant", "content": "Hi there!"},
        }],
        "usage": {"prompt_tokens": 20, "completion_tokens": 5, "total_tokens": 25},
    });
    let (text, usage, outcome) = parse_chat_response(&body).unwrap();
    assert_eq!(text, "Hi there!");
    assert_eq!(usage.total_tokens, Some(25));
    assert_eq!(outcome, TurnOutcome::Completed);
}

#[test]
fn chat_length_finish_maps_to_incomplete() {
    let body = serde_json::json!({
        "choices": [{
            "finish_reason": "length",
            "message": {"role": "assistant", "content": null},
        }],
        "usage": {"prompt_tokens": 19, "completion_tokens": 32, "total_tokens": 51},
    });
    let (text, _, outcome) = parse_chat_response(&body).unwrap();
    assert_eq!(text, "");
    assert_eq!(
        outcome,
        TurnOutcome::Incomplete {
            reason: "max_tokens".to_owned()
        }
    );
}

#[test]
fn parses_live_responses_shape() {
    let body = serde_json::json!({
        "status": "completed",
        "output": [{
            "type": "message",
            "content": [{"type": "output_text", "text": "ok"}],
        }],
        "usage": {"input_tokens": 12, "output_tokens": 175, "total_tokens": 187},
    });
    let (text, usage, outcome) = parse_responses_response(&body).unwrap();
    assert_eq!(text, "ok");
    assert_eq!(usage.total_tokens, Some(187));
    assert_eq!(outcome, TurnOutcome::Completed);
}

#[test]
fn incomplete_responses_keep_reason_with_usage() {
    let body = serde_json::json!({
        "status": "incomplete",
        "incomplete_details": {"reason": "max_output_tokens"},
        "output": [],
        "usage": {"input_tokens": 14, "output_tokens": 64, "total_tokens": 78},
    });
    let (text, usage, outcome) = parse_responses_response(&body).unwrap();
    assert_eq!(text, "");
    assert_eq!(usage.total_tokens, Some(78));
    assert_eq!(
        outcome,
        TurnOutcome::Incomplete {
            reason: "max_output_tokens".to_owned()
        }
    );
}

#[test]
fn failed_responses_are_errors_not_empty_success() {
    let body = serde_json::json!({"status": "failed", "output": []});
    assert!(matches!(
        parse_responses_response(&body),
        Err(DecodeError::TurnFailed)
    ));
}

#[test]
fn parses_live_messages_shape() {
    let body = serde_json::json!({
        "stop_reason": "end_turn",
        "content": [{"type": "text", "text": "Hello!"}],
        "usage": {"input_tokens": 18, "output_tokens": 14},
    });
    let (text, usage, outcome) = parse_messages_response(&body).unwrap();
    assert_eq!(text, "Hello!");
    assert_eq!(usage.total_tokens, Some(32));
    assert_eq!(outcome, TurnOutcome::Completed);
}

#[test]
fn messages_max_tokens_maps_to_incomplete() {
    let body = serde_json::json!({
        "stop_reason": "max_tokens",
        "content": [{"type": "text", "text": "partial"}],
        "usage": {"input_tokens": 18, "output_tokens": 64},
    });
    let (_, _, outcome) = parse_messages_response(&body).unwrap();
    assert_eq!(
        outcome,
        TurnOutcome::Incomplete {
            reason: "max_tokens".to_owned()
        }
    );
}

#[test]
fn messages_error_envelope_is_a_failure() {
    let body = serde_json::json!({"type": "error", "error": {"message": "bad"}});
    assert!(matches!(
        parse_messages_response(&body),
        Err(DecodeError::TurnFailed)
    ));
}

#[test]
fn parses_live_models_list() {
    let ids = parse_models_list(
        r#"{"object":"list","data":[{"id":"glm-5.3-flash"},{"id":"muse-spark-1.3-contributor"}]}"#,
    )
    .unwrap();
    assert_eq!(ids, vec!["glm-5.3-flash", "muse-spark-1.3-contributor"]);
}

#[test]
fn responses_input_preserves_roles() {
    use super::super::{ChatMessage, Role};
    let single = to_responses_input(&[ChatMessage::user("hi")]);
    assert_eq!(single, Value::String("hi".to_owned()));

    let multi = to_responses_input(&[
        ChatMessage {
            role: Role::System,
            content: "be brief".to_owned(),
        },
        ChatMessage::user("hi"),
    ]);
    assert_eq!(
        multi,
        serde_json::json!([
            {"role": "system", "content": "be brief"},
            {"role": "user", "content": "hi"},
        ])
    );
}

#[test]
fn parses_sse_deltas_per_wire_shape() {
    let chat = parse_sse_line(
        "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
        WireProtocol::OpenAiChatCompletions,
    )
    .unwrap();
    assert_eq!(chat.text, "hi");

    let reasoning = parse_sse_line(
        "data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking\"}}]}\n\n",
        WireProtocol::OpenAiChatCompletions,
    )
    .unwrap();
    assert_eq!(reasoning.reasoning, "thinking");

    let msg = parse_sse_line(
            "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
            WireProtocol::AnthropicMessages,
        )
        .unwrap();
    assert_eq!(msg.text, "ok");

    let resp = parse_sse_line(
            "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n",
            WireProtocol::OpenAiResponses,
        )
        .unwrap();
    assert_eq!(resp.text, "ok");

    assert!(parse_sse_line("data: [DONE]\n", WireProtocol::OpenAiChatCompletions).is_none());
}

#[test]
fn truncated_streams_are_errors_per_shape() {
    // No terminal event in any of these bodies.
    let chat = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n";
    let err = process_sse_body(chat, WireProtocol::OpenAiChatCompletions).unwrap_err();
    assert!(matches!(
        err,
        DecodeError::InvalidResponse { detail, .. }
        if detail == "stream ended before terminal event"
    ));

    let msg = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n";
    assert!(process_sse_body(msg, WireProtocol::AnthropicMessages).is_err());

    let resp = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n";
    assert!(process_sse_body(resp, WireProtocol::OpenAiResponses).is_err());
}

#[test]
fn in_stream_provider_errors_fail_per_shape() {
    let chat = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: {\"error\":{\"message\":\"boom\"}}\n\n";
    assert!(matches!(
        process_sse_body(chat, WireProtocol::OpenAiChatCompletions),
        Err(DecodeError::TurnFailed)
    ));

    let msg = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"x\",\"message\":\"boom\"}}\n\n";
    assert!(matches!(
        process_sse_body(msg, WireProtocol::AnthropicMessages),
        Err(DecodeError::TurnFailed)
    ));

    let resp = "event: response.failed\ndata: {\"type\":\"response.failed\",\"response\":{}}\n\n";
    assert!(matches!(
        process_sse_body(resp, WireProtocol::OpenAiResponses),
        Err(DecodeError::TurnFailed)
    ));
}

#[test]
fn complete_native_sequences_resolve() {
    let chat = "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n";
    let (text, _, outcome) = process_sse_body(chat, WireProtocol::OpenAiChatCompletions).unwrap();
    assert_eq!(text, "ok");
    assert_eq!(outcome, TurnOutcome::Completed);

    let msg = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":18,\"output_tokens\":0}}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"ok\"}}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":4}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    let (text, usage, outcome) = process_sse_body(msg, WireProtocol::AnthropicMessages).unwrap();
    assert_eq!(text, "ok");
    assert_eq!(usage.input_tokens, Some(18));
    assert_eq!(usage.output_tokens, Some(4));
    assert_eq!(usage.total_tokens, Some(22));
    assert_eq!(outcome, TurnOutcome::Completed);

    let resp = concat!(
        "event: response.output_text.delta\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n",
        "event: response.completed\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"content\":[{\"type\":\"output_text\",\"text\":\"ok\"}]}],\"usage\":{\"input_tokens\":11,\"output_tokens\":5,\"total_tokens\":16}}}\n\n",
    );
    let (text, usage, outcome) = process_sse_body(resp, WireProtocol::OpenAiResponses).unwrap();
    assert_eq!(text, "ok");
    assert_eq!(usage.total_tokens, Some(16));
    assert_eq!(outcome, TurnOutcome::Completed);
}

#[test]
fn stream_accumulator_survives_split_unicode() {
    let line = "data: {\"choices\":[{\"delta\":{\"content\":\"héllo\"}}]}\n\n";
    for split in 0..line.len() {
        let (head, tail) = line.as_bytes().split_at(split);
        let mut stream = SseStream::new();
        let mut lines = stream.push(head).unwrap();
        lines.extend(stream.push(tail).unwrap());
        lines.extend(stream.finish().unwrap());
        let joined = lines.concat();
        assert!(joined.contains("héllo"), "split at {split}");
    }
}

#[test]
fn oversized_stream_line_is_rejected() {
    let mut stream = SseStream::new();
    let big = vec![b'x'; MAX_SSE_LINE_BYTES + 1];
    assert!(matches!(
        stream.push(&big),
        Err(DecodeError::LimitExceeded { .. })
    ));
}

#[test]
fn anthropic_parts_extract_system() {
    use super::super::{ChatMessage, Role};
    let (system, messages) = to_anthropic_parts(&[
        ChatMessage {
            role: Role::System,
            content: "be brief".to_owned(),
        },
        ChatMessage::user("hi"),
    ]);
    assert_eq!(system.as_deref(), Some("be brief"));
    assert_eq!(messages.len(), 1);
}

#[test]
fn missing_usage_is_unknown_and_reported_zero_is_known() {
    assert_eq!(parse_openai_usage(&serde_json::json!({})), Usage::default());
    let zero = parse_usage(&serde_json::json!({"input_tokens": 0, "output_tokens": 0}));
    assert_eq!(zero.input_tokens, Some(0));
    assert_eq!(zero.output_tokens, Some(0));
    assert_eq!(zero.total_tokens, Some(0));
    assert_eq!(zero.total_source, Some(super::super::UsageSource::Derived));
    let partial = parse_usage(&serde_json::json!({"output_tokens": 8}));
    assert_eq!(partial.input_tokens, None);
    assert_eq!(partial.total_tokens, None);
    let overflowing = Usage::from_reported(Some(u64::MAX), Some(1), None);
    assert_eq!(overflowing.total_tokens, None);
}

#[test]
fn cumulative_usage_preserves_missing_fields_and_reported_totals() {
    let mut usage = Usage::default();
    for value in [
        serde_json::json!({"type": "message_start", "message": {"usage": {"input_tokens": 18, "output_tokens": 1}}}),
        serde_json::json!({"usage": {"output_tokens": 4}}),
        serde_json::json!({"usage": {"output_tokens": 4}}),
    ] {
        merge_sse_usage(&mut usage, &value);
    }
    assert_eq!(usage.total_tokens, Some(22));
    merge_sse_usage(
        &mut usage,
        &serde_json::json!({"usage": {"output_tokens": 0}}),
    );
    assert_eq!(usage.input_tokens, Some(18));
    assert_eq!(usage.output_tokens, Some(0));
    assert_eq!(usage.total_tokens, Some(18));
    merge_sse_usage(
        &mut usage,
        &serde_json::json!({"usage": {"total_tokens": 99}}),
    );
    merge_sse_usage(&mut usage, &serde_json::json!({"usage": {}}));
    assert_eq!(usage.total_tokens, Some(99));
    assert_eq!(
        usage.total_source,
        Some(super::super::UsageSource::Reported)
    );
}

#[test]
fn blocking_responses_require_explicit_terminal_evidence() {
    assert!(
        parse_chat_response(&serde_json::json!({
            "choices": [{"message": {"content": "plausible"}}]
        }))
        .is_err()
    );
    assert!(
        parse_messages_response(&serde_json::json!({
            "content": [{"type": "text", "text": "plausible"}]
        }))
        .is_err()
    );
    assert!(parse_responses_response(&serde_json::json!({"output": []})).is_err());
    assert!(chat_outcome(Some("unrecognized")).is_err());
    assert!(messages_outcome(Some("unrecognized")).is_err());
}

#[test]
fn incomplete_stream_outcomes_survive_trailing_markers() {
    let chat = concat!(
        "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
        "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":64}}\n\n",
        "data: [DONE]\n\n",
    );
    let (text, usage, outcome) =
        process_sse_body(chat, WireProtocol::OpenAiChatCompletions).unwrap();
    assert_eq!(text, "partial");
    assert_eq!(usage.total_tokens, Some(74));
    assert_eq!(outcome, incomplete(Some("max_tokens")));

    let messages = concat!(
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"}}\n\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );
    let (_, _, outcome) = process_sse_body(messages, WireProtocol::AnthropicMessages).unwrap();
    assert_eq!(outcome, incomplete(Some("max_tokens")));
    let responses = "data: {\"type\":\"response.incomplete\",\"response\":{\"status\":\"incomplete\",\"output\":[]}}\n\n";
    let (_, _, outcome) = process_sse_body(responses, WireProtocol::OpenAiResponses).unwrap();
    assert_eq!(outcome, incomplete(None));
}

#[test]
fn bare_end_markers_do_not_prove_success_on_any_wire() {
    for wire in [
        WireProtocol::OpenAiChatCompletions,
        WireProtocol::AnthropicMessages,
        WireProtocol::OpenAiResponses,
    ] {
        assert!(process_sse_body("data: [DONE]\n\n", wire).is_err());
    }
}

#[test]
fn malformed_events_and_post_terminal_content_are_rejected() {
    let terminal = "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n";
    let malformed = format!("data: {{malformed}}\n\n{terminal}");
    assert!(process_sse_body(&malformed, WireProtocol::OpenAiChatCompletions).is_err());
    let late_content =
        format!("{terminal}data: {{\"choices\":[{{\"delta\":{{\"content\":\"late\"}}}}]}}\n\n");
    assert!(process_sse_body(&late_content, WireProtocol::OpenAiChatCompletions).is_err());
    let contradictory = format!(
        "{terminal}data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"length\"}}]}}\n\n"
    );
    assert!(process_sse_body(&contradictory, WireProtocol::OpenAiChatCompletions).is_err());
}

#[test]
fn multiline_sse_events_wait_for_delimiter_and_reject_bad_utf8() {
    let body = concat!(
        "event: chunk\r\n",
        "data: {\"choices\":\r\n",
        "data: [{\"delta\":{\"content\":\"héllo\"},\"finish_reason\":\"stop\"}]}\r\n\r\n",
    );
    for split in 0..body.len() {
        let mut stream = SseStream::new();
        let mut frames = stream.push(&body.as_bytes()[..split]).unwrap();
        frames.extend(stream.push(&body.as_bytes()[split..]).unwrap());
        frames.extend(stream.finish().unwrap());
        assert_eq!(frames.len(), 1);
        let mut fold = StreamFold::default();
        fold.feed(&frames[0], WireProtocol::OpenAiChatCompletions, &mut |_| {})
            .unwrap();
        assert_eq!(fold.text, "héllo");
        assert_eq!(fold.terminal, Some(TurnOutcome::Completed));
    }
    let mut invalid = SseStream::new();
    assert!(invalid.push(b"data: \xff\n\n").is_err());
    assert!(
        process_sse_body(
            "data: {\"choices\":[{\"finish_reason\":\"stop\"}]}\n",
            WireProtocol::OpenAiChatCompletions
        )
        .is_err()
    );
}

#[test]
fn all_sse_line_endings_survive_chunk_boundaries() {
    for ending in ["\n", "\r\n", "\r"] {
        let body = format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"ok\"}},\"finish_reason\":\"stop\"}}]}}{ending}{ending}"
        );
        for split in 0..=body.len() {
            let mut stream = SseStream::new();
            let mut events = stream.push(&body.as_bytes()[..split]).unwrap();
            events.extend(stream.push(&body.as_bytes()[split..]).unwrap());
            events.extend(stream.finish().unwrap());
            assert_eq!(events.len(), 1, "ending {ending:?}, split {split}");
            let mut fold = StreamFold::default();
            fold.feed(&events[0], WireProtocol::OpenAiChatCompletions, &mut |_| {})
                .unwrap();
            assert_eq!(fold.text, "ok");
            assert_eq!(fold.terminal, Some(TurnOutcome::Completed));
        }
    }
}

#[test]
fn structured_content_is_rejected_instead_of_silently_dropped() {
    let chat = serde_json::json!({
        "choices": [{"finish_reason": "stop", "message": {"content": "text", "tool_calls": [{"id": "call-1"}]}}]
    });
    assert!(matches!(
        parse_chat_response(&chat),
        Err(DecodeError::UnsupportedCapability { .. })
    ));
    for kind in ["function_call", "web_search_call", "unrecognized"] {
        let response = serde_json::json!({"status": "completed", "output": [{"type": kind}]});
        assert!(matches!(
            parse_responses_response(&response),
            Err(DecodeError::UnsupportedCapability { .. })
        ));
    }
    let continuation = serde_json::json!({"status": "completed", "output": [{"type": "reasoning", "encrypted_content": "opaque"}]});
    assert!(matches!(
        parse_responses_response(&continuation),
        Err(DecodeError::UnsupportedCapability { .. })
    ));
    let messages = serde_json::json!({"stop_reason": "end_turn", "content": [{"type": "tool_use", "id": "call-1"}]});
    assert!(matches!(
        parse_messages_response(&messages),
        Err(DecodeError::UnsupportedCapability { .. })
    ));
    let stream =
        "data: {\"type\":\"content_block_start\",\"content_block\":{\"type\":\"tool_use\"}}\n\n";
    assert!(matches!(
        process_sse_body(stream, WireProtocol::AnthropicMessages),
        Err(DecodeError::UnsupportedCapability { .. })
    ));
}

#[test]
fn nested_stream_content_is_validated_before_it_can_be_discarded() {
    let cases = [
        (
            WireProtocol::AnthropicMessages,
            serde_json::json!({
                "type": "message_start", "message": {"content": [{"type": "tool_use"}]}
            }),
        ),
        (
            WireProtocol::OpenAiResponses,
            serde_json::json!({
                "type": "response.output_item.done", "item": {"type": "message", "content": [{"type": "refusal", "refusal": "declined"}]}
            }),
        ),
        (
            WireProtocol::OpenAiResponses,
            serde_json::json!({
                "type": "response.content_part.added", "part": {"type": "output_text"}
            }),
        ),
    ];
    for (wire, value) in cases {
        let mut fold = StreamFold::default();
        let mut emitted = false;
        assert!(
            fold.feed(&format!("data: {value}"), wire, &mut |_| emitted = true)
                .is_err()
        );
        assert!(!emitted);
    }
}

#[test]
fn responses_terminal_record_is_authoritative_and_keeps_model_identity() {
    let client = OpencodeClient::new(OpencodeGoProvider::instance(), "synthetic-key").unwrap();
    let response = serde_json::json!({
        "model": "resolved-alias",
        "status": "completed",
        "output": [{"type": "message", "content": [{"type": "output_text", "text": "canonical"}]}]
    });
    let parsed = client
        .blocking_from_value(WireProtocol::OpenAiResponses, "requested-alias", &response)
        .unwrap();
    assert_eq!(parsed.model, "requested-alias");
    assert_eq!(parsed.resolved_model.as_deref(), Some("resolved-alias"));
    let mut fold = StreamFold::default();
    fold.feed(
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"provisional\"}",
        WireProtocol::OpenAiResponses,
        &mut |_| {},
    )
    .unwrap();
    let terminal = format!(
        "data: {}",
        serde_json::json!({"type": "response.completed", "response": response})
    );
    fold.feed(&terminal, WireProtocol::OpenAiResponses, &mut |_| {})
        .unwrap();
    assert_eq!(fold.text, "canonical");
    assert_eq!(fold.resolved_model.as_deref(), Some("resolved-alias"));
    assert_eq!(fold.usage, Usage::default());
}
