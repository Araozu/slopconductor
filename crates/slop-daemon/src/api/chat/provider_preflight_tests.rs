use super::sessions::context_incompatibility;

#[test]
fn rejects_known_incompatible_continuation_but_allows_fresh_go_to_zen_context() {
    assert!(
        context_incompatibility(
            "codex",
            "gpt-6.1-sol",
            "responses",
            false,
            vec![(
                "opencode-go".into(),
                "gpt-6.1-sol".into(),
                "responses".into()
            )],
        )
        .is_some()
    );
    assert!(context_incompatibility("codex", "gpt-6.1-sol", "responses", true, vec![]).is_none());
    assert!(
        context_incompatibility("opencode-zen", "glm-5.3", "chat_completions", true, vec![])
            .is_none()
    );
    assert!(
        context_incompatibility(
            "opencode-zen",
            "glm-5.3",
            "chat_completions",
            false,
            vec![
                (
                    "opencode-zen".into(),
                    "glm-5.3".into(),
                    "chat_completions".into()
                ),
                (
                    "opencode-go".into(),
                    "glm-5.3-flash".into(),
                    "chat_completions".into()
                ),
            ],
        )
        .is_some()
    );
}
