use super::*;

#[test]
fn registry_resolves_implemented_providers() {
    for id in [
        ProviderId::OpencodeGo,
        ProviderId::OpencodeZen,
        ProviderId::Codex,
    ] {
        assert_eq!(provider(id).unwrap().id(), id);
    }
    assert!(provider(ProviderId::OpenAi).is_none());
    assert!(provider(ProviderId::Anthropic).is_none());
}

#[test]
fn chat_request_validation() {
    let base = ChatRequest {
        model: "glm-5.3-flash".to_owned(),
        messages: vec![ChatMessage::user("hi")],
        max_tokens: Some(64),
        session_id: "test-session_1".to_owned(),
    };
    base.validate().unwrap();

    let empty = ChatRequest {
        messages: Vec::new(),
        ..base.clone()
    };
    assert!(empty.validate().is_err());

    let bad_tokens = ChatRequest {
        max_tokens: Some(0),
        ..base.clone()
    };
    assert!(bad_tokens.validate().is_err());

    let bad_session = ChatRequest {
        session_id: "has space".to_owned(),
        ..base.clone()
    };
    assert!(bad_session.validate().is_err());
}

#[test]
fn chat_request_memory_bounds() {
    let base = ChatRequest {
        model: "glm-5.3-flash".to_owned(),
        messages: vec![ChatMessage::user("hi")],
        max_tokens: Some(64),
        session_id: "s".to_owned(),
    };
    let many = ChatRequest {
        messages: vec![ChatMessage::user("hi"); ChatRequest::MAX_MESSAGES + 1],
        ..base.clone()
    };
    assert!(matches!(
        many.validate(),
        Err(ProviderError::LimitExceeded { .. })
    ));

    let big = ChatRequest {
        messages: vec![ChatMessage {
            role: Role::User,
            content: "x".repeat(ChatRequest::MAX_REQUEST_BYTES + 1),
        }],
        ..base.clone()
    };
    assert!(matches!(
        big.validate(),
        Err(ProviderError::LimitExceeded { .. })
    ));
}
