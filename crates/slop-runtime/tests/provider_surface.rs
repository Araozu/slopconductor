//! Hermetic checks of the current provider-neutral one-turn execution boundary.

use slop_runtime::providers::{
    ChatMessage, ChatRequest, ChatResponse, OpencodeGoProvider, Provider, ProviderClient,
    ProviderError, ProviderFuture, Role, StreamDelta, TurnOutcome, Usage,
    opencode_go::OpencodeGoClient,
};

struct FixtureClient {
    text: &'static str,
    outcome: TurnOutcome,
}

impl ProviderClient for FixtureClient {
    fn descriptor(&self) -> &dyn Provider {
        OpencodeGoProvider::instance()
    }

    fn list_models(&self) -> ProviderFuture<'_, Vec<String>> {
        Box::pin(async { Ok(vec!["glm-5.3-flash".to_owned()]) })
    }

    fn complete<'a>(&'a self, request: &'a ChatRequest) -> ProviderFuture<'a, ChatResponse> {
        Box::pin(async move {
            let wire = self.validate(request)?;
            Ok(ChatResponse {
                model: request.model.clone(),
                resolved_model: Some("fixture-resolved-model".to_owned()),
                text: self.text.to_owned(),
                usage: Usage::default(),
                wire,
                outcome: self.outcome.clone(),
            })
        })
    }

    fn complete_streaming<'a>(
        &'a self,
        request: &'a ChatRequest,
        on_delta: &'a mut (dyn FnMut(StreamDelta) + Send),
    ) -> ProviderFuture<'a, ChatResponse> {
        Box::pin(async move {
            let result = self.complete(request).await?;
            for character in result.text.chars() {
                on_delta(StreamDelta {
                    text: character.to_string(),
                    reasoning: String::new(),
                });
            }
            Ok(result)
        })
    }
}

fn request(model: &str) -> ChatRequest {
    ChatRequest {
        model: model.to_owned(),
        messages: vec![ChatMessage::user("fixture prompt")],
        max_tokens: Some(64),
        session_id: "fixture-session".to_owned(),
    }
}

fn require_send<T: Send>(value: T) -> T {
    value
}

#[tokio::test]
async fn one_consumer_handles_complete_and_incomplete_streams() {
    let fixtures = [
        FixtureClient {
            text: "héllo",
            outcome: TurnOutcome::Completed,
        },
        FixtureClient {
            text: "partial",
            outcome: TurnOutcome::Incomplete {
                reason: "max_tokens".to_owned(),
            },
        },
    ];
    for fixture in &fixtures {
        let client: &dyn ProviderClient = fixture;
        let models = require_send(client.list_models()).await.unwrap();
        let request = request(&models[0]);
        let non_streaming = require_send(client.complete(&request)).await.unwrap();
        let mut visible = String::new();
        let streaming = require_send(
            client.complete_streaming(&request, &mut |delta| visible.push_str(&delta.text)),
        )
        .await
        .unwrap();
        assert_eq!(streaming, non_streaming);
        assert_eq!(visible, streaming.text);
        assert_eq!(streaming.usage.total_tokens, None);
        assert_eq!(
            streaming.resolved_model.as_deref(),
            Some("fixture-resolved-model")
        );
    }
}

#[tokio::test]
async fn real_and_fixture_clients_reject_invalid_requests_before_dispatch() {
    let real = OpencodeGoClient::new("synthetic-not-a-live-credential").unwrap();
    let fixture = FixtureClient {
        text: "unused",
        outcome: TurnOutcome::Completed,
    };
    for client in [
        &real as &dyn ProviderClient,
        &fixture as &dyn ProviderClient,
    ] {
        let mut invalid = request("glm-5.3-flash");
        invalid.messages.clear();
        assert!(matches!(
            require_send(client.complete(&invalid)).await,
            Err(ProviderError::InvalidRequest(_))
        ));
        let mut emitted = false;
        assert!(matches!(
            require_send(client.complete_streaming(&invalid, &mut |_| emitted = true)).await,
            Err(ProviderError::InvalidRequest(_))
        ));
        assert!(!emitted);

        let mut misplaced = request("claude-haiku-5-5");
        misplaced.messages.push(ChatMessage {
            role: Role::System,
            content: "later instruction".to_owned(),
        });
        assert!(matches!(
            client.validate(&misplaced),
            Err(ProviderError::UnsupportedCapability { .. })
        ));
    }
}
