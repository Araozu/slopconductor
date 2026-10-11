#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TurnControl {
    pub cancellation_requested: bool,
    pub immediate_requested: bool,
    pub pause_requested: bool,
    pub steering_pending: bool,
    pub budget_exhausted: bool,
    pub waiting_requested: bool,
}
