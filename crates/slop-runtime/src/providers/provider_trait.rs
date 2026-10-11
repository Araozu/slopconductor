use slop_core::provider::{ProviderId, is_valid_model_id};

use super::{AuthMode, ProviderError, ProviderModel, WireProtocol};

/// A compiled-in model provider.
pub trait Provider: Send + Sync {
    fn id(&self) -> ProviderId;
    fn display_name(&self) -> &'static str;
    fn base_url(&self) -> &'static str;
    /// Environment variable holding this provider's API key.
    fn env_key_var(&self) -> &'static str;
    fn models(&self) -> &'static [ProviderModel];

    fn auth_modes(&self) -> &'static [AuthMode] {
        &[AuthMode::ApiKey]
    }

    /// Whether the current integration requires an explicit output-token limit.
    fn requires_output_limit(&self) -> bool {
        true
    }

    /// Look up which wire shape serves `model`, rejecting unknown ids.
    fn wire_protocol(&self, model: &str) -> Result<WireProtocol, ProviderError> {
        if !is_valid_model_id(model) {
            return Err(ProviderError::InvalidModel {
                provider: self.id(),
                model: model.to_owned(),
            });
        }
        self.models()
            .iter()
            .find(|m| m.id == model)
            .map(|m| m.wire)
            .ok_or_else(|| ProviderError::InvalidModel {
                provider: self.id(),
                model: model.to_owned(),
            })
    }
}
