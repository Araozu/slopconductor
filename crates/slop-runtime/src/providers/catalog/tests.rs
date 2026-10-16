use serde_json::json;

use super::*;
use crate::providers::test_http::{Reply, Server};

fn fixture() -> Value {
    let mut value = json!({
        "opencode-go": {"npm":"@ai-sdk/openai-compatible", "models": {
            "fresh-chat": {"id":"fresh-chat","name":"Fresh chat","reasoning":true,"tool_call":true,
                "limit":{"context":123456,"input":120000},
                "reasoning_options":[{"type":"toggle"},{"type":"effort","values":["low","max","max",null,"default","bad value","ultra"]}]},
            "fixed": {"id":"fixed","name":"Fixed reasoning","reasoning":true,"reasoning_options":[]},
            "budget": {"id":"budget","name":"Budget only","reasoning":true,"reasoning_options":[{"type":"budget_tokens"}]},
            "toggle": {"id":"toggle","name":"Toggle only","reasoning":true,"reasoning_options":[{"type":"toggle"}]},
            "google": {"id":"google","name":"Unsupported wire","reasoning":true,"provider":{"npm":"@ai-sdk/google"},"reasoning_options":[{"type":"effort","values":["high"]}]}
        }},
        "opencode": {"npm":"@ai-sdk/openai-compatible", "models": {
            "fresh-chat": {"id":"fresh-chat","name":"Same ID, different API","reasoning":true,"provider":{"npm":"@ai-sdk/anthropic"},
                "reasoning_options":[{"type":"effort","values":["medium","xhigh"]}]},
            "fresh-response": {"id":"fresh-response","name":"New Responses model","reasoning":true,"provider":{"npm":"@ai-sdk/openai"},
                "reasoning_options":[{"type":"effort","values":["none","high"]}]}
        }}
    });
    for provider in ["opencode-go", "opencode"] {
        for model in value[provider]["models"]
            .as_object_mut()
            .unwrap()
            .values_mut()
        {
            model["modalities"] = json!({"output":["text"]});
        }
    }
    value
}

#[test]
fn explicit_efforts_and_wire_formats_are_provider_scoped() {
    let catalog = parse(&serde_json::to_vec(&fixture()).unwrap()).unwrap();
    let go = &catalog.0[&ProviderId::OpencodeGo];
    let zen = &catalog.0[&ProviderId::OpencodeZen];
    assert_eq!(
        go["fresh-chat"].wire,
        Some(WireProtocol::OpenAiChatCompletions)
    );
    assert_eq!(
        go["fresh-chat"].capabilities.reasoning_efforts,
        ["low", "max", "ultra"]
    );
    assert_eq!(go["fresh-chat"].capabilities.max_input_tokens, Some(120000));
    assert_eq!(
        go["fresh-chat"].capabilities.context_window_tokens,
        Some(123456)
    );
    assert!(go["fresh-chat"].capabilities.tools);
    for id in ["fixed", "budget", "toggle", "google"] {
        assert!(go[id].capabilities.reasoning_efforts.is_empty(), "{id}");
    }
    assert_eq!(go["google"].wire, None);
    assert_eq!(
        zen["fresh-chat"].wire,
        Some(WireProtocol::AnthropicMessages)
    );
    assert_eq!(
        zen["fresh-chat"].capabilities.reasoning_efforts,
        ["medium", "xhigh"]
    );
    assert_eq!(
        zen["fresh-response"].wire,
        Some(WireProtocol::OpenAiResponses)
    );
    assert_eq!(
        zen["fresh-response"].capabilities.reasoning_efforts,
        ["none", "high"]
    );
}

#[test]
fn bundled_catalog_exposes_gateway_efforts_without_network() {
    let go = super::super::OpencodeGoProvider::instance();
    let zen = super::super::OpencodeZenProvider::instance();
    assert_eq!(
        capabilities(go.id(), "glm-5.3-flash")
            .unwrap()
            .reasoning_efforts,
        ["low", "high", "max"]
    );
    assert_eq!(
        capabilities(zen.id(), "gpt-6.1-sol")
            .unwrap()
            .reasoning_efforts,
        ["low", "medium", "high", "xhigh", "max"]
    );
    assert!(
        capabilities(zen.id(), "big-pickle")
            .unwrap()
            .reasoning_efforts
            .is_empty()
    );
    // New, explicitly mapped IDs are executable without adding a Rust constant.
    assert_eq!(
        go.wire_protocol("cat-jam-free").unwrap(),
        WireProtocol::OpenAiChatCompletions
    );
    assert!(models(go).iter().any(|m| m.id == "cat-jam-free"));
    assert!(zen.wire_protocol("gemini-4-argon").is_err());
}

#[test]
fn invalid_or_partial_catalog_cannot_replace_last_good_data() {
    for value in [
        json!({}),
        json!({"opencode-go":{"models":{}}}),
        json!({"opencode":{}}),
    ] {
        assert!(parse(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    let mut value = fixture();
    value["opencode"]["models"]["fresh-response"]["id"] = json!("mismatched-id");
    let catalog = parse(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(!catalog.0[&ProviderId::OpencodeZen].contains_key("fresh-response"));
}

#[test]
fn new_models_require_explicit_text_output_and_well_formed_api_mappings() {
    let mut value = fixture();
    value["opencode-go"]["models"]["fresh-chat"]["modalities"] = json!({"output":["image"]});
    value["opencode"]["models"]["fresh-response"]["provider"]["npm"] = json!(42);
    let catalog = parse(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(catalog.0[&ProviderId::OpencodeGo]["fresh-chat"].wire, None);
    assert_eq!(
        catalog.0[&ProviderId::OpencodeZen]["fresh-response"].wire,
        None
    );
    value["opencode"]["npm"] = Value::Null;
    assert!(parse(&serde_json::to_vec(&value).unwrap()).is_err());
}

#[tokio::test]
async fn refresh_transport_is_bounded_and_never_sends_provider_credentials_or_retries() {
    let server = Server::new(vec![
        Reply::json(fixture()),
        Reply {
            status: reqwest::StatusCode::FOUND,
            content_type: "application/json",
            body: String::new(),
            location: Some("/must-not-follow".into()),
        },
        Reply::json(json!({})),
        Reply {
            status: reqwest::StatusCode::OK,
            content_type: "application/json",
            body: "x".repeat(MAX_CATALOG_BYTES + 1),
            location: None,
        },
    ])
    .await;
    let url = format!("{}/api.json", server.url);
    let catalog = fetch(&url).await.unwrap();
    assert_eq!(
        catalog.0[&ProviderId::OpencodeGo]["fresh-chat"]
            .capabilities
            .reasoning_efforts,
        ["low", "max", "ultra"]
    );
    assert!(fetch(&url).await.is_err());
    assert!(fetch(&url).await.is_err());
    assert!(matches!(
        fetch(&url).await,
        Err(ProviderError::LimitExceeded { .. })
    ));
    server.inspect(|requests| {
        assert_eq!(requests.len(), 4);
        for request in requests {
            assert_eq!(request.path, "/api.json");
            assert!(!request.headers.contains_key("authorization"));
            assert!(!request.headers.contains_key("x-api-key"));
            assert!(request.body.is_empty());
        }
    });
}
