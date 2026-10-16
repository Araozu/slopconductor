use super::super::Provider;
use super::ModelCapabilities;

pub fn capabilities(provider: &dyn Provider, model: &str) -> ModelCapabilities {
    if provider.id() == slop_core::provider::ProviderId::Codex {
        return super::super::codex::model_capabilities(model);
    }
    if let Some(capabilities) = super::super::catalog::capabilities(provider.id(), model) {
        return capabilities;
    }
    // Models missing from public metadata retain their compiled mapping without
    // inventing effort controls from a model name or a reasoning boolean.
    let (context_window_tokens, max_input_tokens) =
        super::super::context_limits::limits(provider.id(), model);
    ModelCapabilities {
        tools: true,
        reasoning_efforts: Vec::new(),
        context_window_tokens,
        max_input_tokens,
    }
}
