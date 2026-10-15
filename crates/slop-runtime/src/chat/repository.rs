use std::{error::Error, future::Future, pin::Pin};

use crate::{
    agent::{RequestCompletion, RequestIntent, ToolIntent},
    providers::inference::InferenceMessage,
    tools::ToolOutcome,
};

use super::{ChatOutcome, TurnControl, TurnWork};

pub type RepoError = Box<dyn Error + Send + Sync>;
pub type RepoFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, RepoError>> + Send + 'a>>;

/// Storage boundary for accepted chat turns. Implementations own transaction
/// semantics and wire-independent persistence; the runtime never sees SQL/DTOs.
pub trait ChatRepository: Send + Sync + 'static {
    fn orchestration_tool<'a>(
        &'a self,
        _turn_id: &'a str,
        _intent: &'a ToolIntent,
    ) -> RepoFuture<'a, crate::orchestration::OrchestrationResult> {
        Box::pin(async {
            Ok(crate::orchestration::OrchestrationResult::Completed(
                ToolOutcome::failed("orchestration_unavailable", false),
            ))
        })
    }
    /// Validate the selected workspace after admission. A committed Git
    /// failure is a turn outcome; persistence failures stop the supervisor.
    fn prepare_workspace<'a>(
        &'a self,
        _turn_id: &'a str,
        _shutdown: &'a mut tokio::sync::watch::Receiver<bool>,
    ) -> RepoFuture<'a, Option<(&'static str, &'static str)>> {
        Box::pin(async { Ok(None) })
    }
    fn begin_request<'a>(&'a self, turn_id: &'a str, intent: RequestIntent)
    -> RepoFuture<'a, bool>;
    fn checkpoint_request<'a>(
        &'a self,
        turn_id: &'a str,
        request_id: &'a str,
        message: InferenceMessage,
    ) -> RepoFuture<'a, ()>;
    /// Commit the completed message and every tool intent atomically before dispatch.
    fn complete_request<'a>(
        &'a self,
        turn_id: &'a str,
        completion: RequestCompletion,
    ) -> RepoFuture<'a, ()>;
    fn fail_request<'a>(
        &'a self,
        turn_id: &'a str,
        request_id: &'a str,
        code: &'a str,
    ) -> RepoFuture<'a, ()>;
    fn start_tool<'a>(&'a self, turn_id: &'a str, intent: &'a ToolIntent) -> RepoFuture<'a, bool>;
    fn finish_tool<'a>(
        &'a self,
        turn_id: &'a str,
        intent: &'a ToolIntent,
        outcome: ToolOutcome,
    ) -> RepoFuture<'a, ()>;
    /// Atomically claim the next eligible turn and mark it running.
    fn claim_next(&self) -> RepoFuture<'_, Option<TurnWork>>;
    /// Reconcile durable deadlines and notifications even while every execution
    /// permit is occupied.
    fn reconcile(&self) -> RepoFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
    /// Persist bounded visible assistant text as an incomplete checkpoint.
    fn checkpoint_visible<'a>(&'a self, turn_id: &'a str, text: &'a str) -> RepoFuture<'a, ()>;
    /// Atomically record the terminal assistant message, usage, outcome, and
    /// semantic event before the caller considers the turn complete.
    fn finish_turn<'a>(&'a self, turn_id: &'a str, outcome: ChatOutcome) -> RepoFuture<'a, ()>;
    /// Read the durable cancellation bit. Runtime polls it while inference is
    /// in flight; there is no automatic replay after interruption.
    fn cancellation_requested<'a>(&'a self, turn_id: &'a str) -> RepoFuture<'a, bool>;
    /// Read durable boundary controls. A pending instruction never enters
    /// context until `finish_turn` atomically applies it and queues a replan.
    fn turn_control<'a>(&'a self, turn_id: &'a str) -> RepoFuture<'a, TurnControl>;
    /// A queued turn is a same-process continuation only after boundary steering.
    fn retain_provider_snapshot<'a>(&'a self, turn_id: &'a str) -> RepoFuture<'a, bool>;
}
