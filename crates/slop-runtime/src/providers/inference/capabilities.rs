use super::super::Provider;
use super::ModelCapabilities;

pub fn capabilities(provider: &dyn Provider, model: &str) -> ModelCapabilities {
    if provider.id() == slop_core::provider::ProviderId::Codex {
        return super::super::codex::model_capabilities(model);
    }
    // Gateway adapters implement all three wire formats. Gateway-specific
    // reasoning settings remain unadvertised until verified for each model.
    ModelCapabilities {
        tools: true,
        reasoning_efforts: Vec::new(),
    }
}
