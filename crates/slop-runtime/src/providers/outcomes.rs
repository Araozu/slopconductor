use super::{Usage, WireProtocol};

/// Provider-neutral terminal outcome for a turn.
///
/// Reasoning models can exhaust `max_tokens` on thinking before producing
/// visible text; that `Incomplete` state is recorded with the provider's
/// reason instead of masquerading as success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    Completed,
    Incomplete { reason: String },
}

/// Provider-neutral completed turn with the recorded usage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatResponse {
    /// Requested model selection. The upstream may resolve an alias differently.
    pub model: String,
    /// Actual model reported upstream, or unknown when omitted.
    pub resolved_model: Option<String>,
    pub text: String,
    pub usage: Usage,
    pub wire: WireProtocol,
    pub outcome: TurnOutcome,
}

/// One streaming delta from an inference request.
///
/// `text` carries visible assistant deltas; `reasoning` carries
/// provider-specific thinking deltas (e.g. `reasoning_content`). Either may be
/// empty; both empty means a keep-alive frame.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StreamDelta {
    pub text: String,
    pub reasoning: String,
}
