use std::{fmt, str::FromStr};

use super::{ProviderId, ProviderIdentityError};

/// Maximum length for an opaque model identifier.
pub const MAX_MODEL_ID_LEN: usize = 128;

/// Validate an opaque model identifier without interpreting it.
///
/// Accepts the provider-style ids used by OpenCode Go (e.g.
/// `muse-spark-1.3-contributor`, `qwen3.8-max`, `kimi-k2.7-code`): ASCII
/// letters, digits, `.`, `-`, `_`, `/`, and `:`.
#[must_use]
pub fn is_valid_model_id(model: &str) -> bool {
    if model.is_empty() || model.len() > MAX_MODEL_ID_LEN {
        return false;
    }
    model
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '/' | ':'))
}

/// A validated provider/model reference, e.g. `opencode-go/glm-5.3-flash`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProviderModelRef {
    provider: ProviderId,
    model: String,
}

impl ProviderModelRef {
    /// Validate and store a provider/model pair.
    pub fn new(provider: ProviderId, model: &str) -> Result<Self, ProviderIdentityError> {
        if !is_valid_model_id(model) {
            return Err(ProviderIdentityError("invalid model id"));
        }
        Ok(Self {
            provider,
            model: model.to_owned(),
        })
    }

    #[must_use]
    pub fn provider(&self) -> ProviderId {
        self.provider
    }

    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }
}

impl fmt::Display for ProviderModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.provider, self.model)
    }
}

impl FromStr for ProviderModelRef {
    type Err = ProviderIdentityError;

    /// Parse `provider/model`, where model itself may contain `/`.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (provider, model) = value
            .split_once('/')
            .ok_or(ProviderIdentityError("expected provider/model"))?;
        Self::new(provider.parse()?, model)
    }
}
