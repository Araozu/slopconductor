use hive_core::provider::ProviderId;

use super::super::inference::ModelCapabilities;
use super::super::{AuthMode, Provider, ProviderModel, WireProtocol};

pub const BASE_URL: &str = "https://api.openai.com/v1";
pub const ENV_KEY_VAR: &str = "OPENAI_API_KEY";

/// Supported Responses settings, checked against the model pages on 2026-10-11.
pub fn model_capabilities(model: &str) -> ModelCapabilities {
    let efforts: &[&str] = match model {
        "gpt-6-sol" | "gpt-6-luna" => &["none", "low", "medium", "high", "xhigh", "max"],
        "gpt-6.1-sol" | "gpt-6-astra" => &["low", "medium", "high", "xhigh", "max"],
        _ => &[],
    };
    let (context_window_tokens, max_input_tokens) =
        super::super::context_limits::limits(ProviderId::Codex, model);
    ModelCapabilities {
        tools: !efforts.is_empty(),
        reasoning_efforts: efforts.iter().map(|effort| (*effort).to_owned()).collect(),
        context_window_tokens,
        max_input_tokens,
    }
}

/// Explicit Responses mappings, verified against official docs on 2026-10-08.
/// This catalog declares adapter support; account entitlement is checked by
/// the upstream service. Live discovery does not enable unknown model ids.
pub const MODELS: &[ProviderModel] = &[
    model("gpt-6.1-sol", "GPT 6.1 Sol"),
    model("gpt-6-sol", "GPT 6 Sol"),
    model("gpt-6-luna", "GPT 6 Luna"),
    model("gpt-6-astra", "GPT 6 Astra"),
];

const fn model(id: &'static str, display_name: &'static str) -> ProviderModel {
    ProviderModel {
        id,
        display_name,
        wire: WireProtocol::OpenAiResponses,
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CodexProvider;

impl CodexProvider {
    #[must_use]
    pub fn instance() -> &'static Self {
        static INSTANCE: CodexProvider = CodexProvider;
        &INSTANCE
    }
}

impl Provider for CodexProvider {
    fn id(&self) -> ProviderId {
        ProviderId::Codex
    }

    fn display_name(&self) -> &'static str {
        "Codex"
    }

    fn base_url(&self) -> &'static str {
        BASE_URL
    }

    fn env_key_var(&self) -> &'static str {
        ENV_KEY_VAR
    }

    fn models(&self) -> &'static [ProviderModel] {
        MODELS
    }

    fn auth_modes(&self) -> &'static [AuthMode] {
        &[AuthMode::ChatGptSubscription, AuthMode::ApiKey]
    }

    fn requires_output_limit(&self) -> bool {
        false
    }
}
