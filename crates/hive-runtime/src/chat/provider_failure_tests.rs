use super::*;
use hive_core::provider::ProviderId;

#[test]
fn provider_failures_retain_safe_actionable_causes() {
    for (error, code, detail) in [
        (
            ProviderError::UnexpectedStatus {
                provider: ProviderId::Codex,
                status: 400,
            },
            "provider_http_error",
            "HTTP 400",
        ),
        (
            ProviderError::InvalidResponse {
                provider: ProviderId::Codex,
                detail: "stream ended before terminal event",
            },
            "provider_invalid_response",
            "stream ended before terminal event",
        ),
        (
            ProviderError::Authentication {
                detail: "token renewal requires a new sign-in",
            },
            "provider_auth_failed",
            "token renewal requires a new sign-in",
        ),
        (
            ProviderError::PlanUsage {
                code: "subscription_sharing_usage_unavailable",
            },
            "subscription_sharing_usage_unavailable",
            "Keep this login",
        ),
        (
            ProviderError::PlanUsage {
                code: "subscription_sharing_usage_limit_exceeded",
            },
            "subscription_sharing_usage_limit_exceeded",
            "Settings > Usage",
        ),
    ] {
        let outcome = provider_failure(error);
        assert_eq!(outcome.error_code.as_deref(), Some(code));
        assert!(outcome.error_message.as_deref().unwrap().contains(detail));
    }
}
