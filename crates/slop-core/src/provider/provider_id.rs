use std::{fmt, str::FromStr};

use super::ProviderIdentityError;

/// Static set of supported model providers.
///
/// The registry is intentionally closed: providers are compiled in, not loaded
/// as runtime plugins. Adding a provider means adding a variant plus its
/// runtime integration and model catalog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderId {
    /// OpenCode Go subscription gateway (`OPENCODE_GO_API_KEY`).
    OpencodeGo,
    /// OpenCode Zen pay-as-you-go gateway (`OPENCODE_ZEN_API_KEY`).
    OpencodeZen,
    /// Direct OpenAI API integration (future).
    OpenAi,
    /// Direct Anthropic API integration (future).
    Anthropic,
    /// Headless Codex model access through the public OpenAI Responses API.
    Codex,
}

impl ProviderId {
    /// Stable wire/config string, e.g. `opencode-go`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OpencodeGo => "opencode-go",
            Self::OpencodeZen => "opencode-zen",
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::Codex => "codex",
        }
    }

    /// All providers compiled into this build, in registry order.
    #[must_use]
    pub fn all() -> &'static [Self] {
        &[
            Self::OpencodeGo,
            Self::OpencodeZen,
            Self::OpenAi,
            Self::Anthropic,
            Self::Codex,
        ]
    }
}

impl fmt::Display for ProviderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ProviderId {
    type Err = ProviderIdentityError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "opencode-go" => Ok(Self::OpencodeGo),
            "opencode-zen" => Ok(Self::OpencodeZen),
            "openai" => Ok(Self::OpenAi),
            "anthropic" => Ok(Self::Anthropic),
            "codex" => Ok(Self::Codex),
            _ => Err(ProviderIdentityError("unknown provider id")),
        }
    }
}
