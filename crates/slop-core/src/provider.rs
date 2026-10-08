//! Provider and model identities for Slop Conductor.
//!
//! These are transport-independent domain values. HTTP endpoints, credentials,
//! and wire payloads belong in `slop-runtime`; versioned DTOs belong in
//! `slop-protocol`. This module only names the static set of supported
//! providers and validates opaque model identifiers.

use std::{fmt, str::FromStr};

/// Static set of supported model providers.
///
/// The registry is intentionally closed: providers are compiled in, not loaded
/// as runtime plugins. Adding a provider means adding a variant plus its
/// runtime integration and model catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderId {
    /// OpenCode Go subscription gateway (`OPENCODE_GO_API_KEY`).
    OpencodeGo,
    /// Direct OpenAI API integration (future).
    OpenAi,
    /// Direct Anthropic API integration (future).
    Anthropic,
    /// Codex-style Responses integration with its own session handling (future).
    Codex,
}

impl ProviderId {
    /// Stable wire/config string, e.g. `opencode-go`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpencodeGo => "opencode-go",
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Codex => "codex",
        }
    }

    /// All providers compiled into this build, in registry order.
    #[must_use]
    pub fn all() -> &'static [Self] {
        &[Self::OpencodeGo, Self::OpenAi, Self::Anthropic, Self::Codex]
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error for unknown provider names or invalid model identifiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderIdentityError(pub &'static str);

impl fmt::Display for ProviderIdentityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for ProviderIdentityError {}

impl FromStr for ProviderId {
    type Err = ProviderIdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "opencode-go" => Ok(Self::OpencodeGo),
            "openai" => Ok(Self::OpenAi),
            "anthropic" => Ok(Self::Anthropic),
            "codex" => Ok(Self::Codex),
            _ => Err(ProviderIdentityError("unknown provider id")),
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_ids_round_trip() {
        for id in ProviderId::all() {
            assert_eq!(id.as_str().parse::<ProviderId>().as_ref(), Ok(id));
        }
        assert!("azure".parse::<ProviderId>().is_err());
    }

    #[test]
    fn model_id_validation() {
        for valid in [
            "glm-5.3-flash",
            "muse-spark-1.3-contributor",
            "qwen3.8-max",
            "kimi-k2.7-code",
            "org/model:v1",
        ] {
            assert!(is_valid_model_id(valid), "{valid}");
        }
        for invalid in ["", "has space", "uniçode", "semi;colon", "quote\""] {
            assert!(!is_valid_model_id(invalid), "{invalid:?}");
        }
        assert!(!is_valid_model_id(&"m".repeat(MAX_MODEL_ID_LEN + 1)));
    }

    #[test]
    fn model_ref_parse_and_display() {
        let parsed: ProviderModelRef = "opencode-go/glm-5.3-flash".parse().unwrap();
        assert_eq!(parsed.provider(), ProviderId::OpencodeGo);
        assert_eq!(parsed.model(), "glm-5.3-flash");
        assert_eq!(parsed.to_string(), "opencode-go/glm-5.3-flash");
        assert!("opencode-go/".parse::<ProviderModelRef>().is_err());
        assert!("no-slash".parse::<ProviderModelRef>().is_err());
        assert!("unknown/model".parse::<ProviderModelRef>().is_err());
    }
}
