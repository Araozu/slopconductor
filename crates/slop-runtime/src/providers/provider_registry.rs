use slop_core::provider::ProviderId;

use super::{CodexProvider, OpencodeGoProvider, OpencodeZenProvider, Provider};

/// Look up a compiled-in provider by id.
///
/// OpenCode Go, Zen, and Codex have runtime integrations; the remaining
/// [`ProviderId`] variants are reserved so callers can match on the same
/// structure without a plugin mechanism.
#[must_use]
pub fn provider(id: ProviderId) -> Option<&'static dyn Provider> {
    match id {
        ProviderId::OpencodeGo => Some(OpencodeGoProvider::instance()),
        ProviderId::OpencodeZen => Some(OpencodeZenProvider::instance()),
        ProviderId::Codex => Some(CodexProvider::instance()),
        ProviderId::OpenAi | ProviderId::Anthropic => None,
    }
}
