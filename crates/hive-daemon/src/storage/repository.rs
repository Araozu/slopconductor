use super::*;

impl hive_runtime::chat::ChatRepository for StoreClient {
    fn orchestration_tool<'a>(
        &'a self,
        turn_id: &'a str,
        intent: &'a hive_runtime::agent::ToolIntent,
    ) -> hive_runtime::chat::RepoFuture<'a, hive_runtime::orchestration::OrchestrationResult> {
        Box::pin(async move {
            match self.native_orchestration(turn_id, intent).await {
                Ok(result) => Ok(result),
                Err(error) => {
                    let code = match error {
                        StoreError::Invalid => "invalid_arguments",
                        StoreError::NotFound => "child_not_found",
                        StoreError::Conflict => "child_state_conflict",
                        StoreError::Limit => "child_limit",
                        StoreError::Unsupported(_) => "child_not_allowed",
                        _ => return Err(Box::new(error) as hive_runtime::chat::RepoError),
                    };
                    Ok(hive_runtime::orchestration::OrchestrationResult::Completed(
                        hive_runtime::tools::ToolOutcome::failed(code, false),
                    ))
                }
            }
        })
    }
    fn prepare_workspace<'a>(
        &'a self,
        turn_id: &'a str,
        shutdown: &'a mut tokio::sync::watch::Receiver<bool>,
    ) -> hive_runtime::chat::RepoFuture<'a, Option<(&'static str, &'static str)>> {
        Box::pin(async move {
            self.prepare_workspace(turn_id, shutdown)
                .await
                .map_err(|error| Box::new(error) as hive_runtime::chat::RepoError)
        })
    }
    fn begin_request<'a>(
        &'a self,
        turn_id: &'a str,
        intent: hive_runtime::agent::RequestIntent,
    ) -> hive_runtime::chat::RepoFuture<'a, bool> {
        Box::pin(async move {
            let turn = turn_id.to_owned();
            self.submit(move |c| execution::begin_request(c, &turn, intent))
                .await
                .map_err(|e| Box::new(e) as hive_runtime::chat::RepoError)
        })
    }
    fn checkpoint_request<'a>(
        &'a self,
        turn_id: &'a str,
        request_id: &'a str,
        message: hive_runtime::providers::inference::InferenceMessage,
    ) -> hive_runtime::chat::RepoFuture<'a, ()> {
        Box::pin(async move {
            let turn = turn_id.to_owned();
            let request = request_id.to_owned();
            self.submit(move |c| execution::checkpoint_request(c, &turn, &request, message))
                .await
                .map_err(|e| Box::new(e) as hive_runtime::chat::RepoError)
        })
    }
    fn complete_request<'a>(
        &'a self,
        turn_id: &'a str,
        completion: hive_runtime::agent::RequestCompletion,
    ) -> hive_runtime::chat::RepoFuture<'a, ()> {
        Box::pin(async move {
            let turn = turn_id.to_owned();
            self.submit(move |c| execution::complete_request(c, &turn, completion))
                .await
                .map_err(|e| Box::new(e) as hive_runtime::chat::RepoError)
        })
    }
    fn fail_request<'a>(
        &'a self,
        turn_id: &'a str,
        request_id: &'a str,
        code: &'a str,
    ) -> hive_runtime::chat::RepoFuture<'a, ()> {
        Box::pin(async move {
            let turn = turn_id.to_owned();
            let request = request_id.to_owned();
            let code = code.to_owned();
            self.submit(move |c| execution::fail_request(c, &turn, &request, &code))
                .await
                .map_err(|e| Box::new(e) as hive_runtime::chat::RepoError)
        })
    }
    fn start_tool<'a>(
        &'a self,
        turn_id: &'a str,
        intent: &'a hive_runtime::agent::ToolIntent,
    ) -> hive_runtime::chat::RepoFuture<'a, bool> {
        Box::pin(async move {
            let turn = turn_id.to_owned();
            let intent = intent.clone();
            self.submit(move |c| execution::start_tool(c, &turn, &intent))
                .await
                .map_err(|e| Box::new(e) as hive_runtime::chat::RepoError)
        })
    }
    fn finish_tool<'a>(
        &'a self,
        turn_id: &'a str,
        intent: &'a hive_runtime::agent::ToolIntent,
        outcome: hive_runtime::tools::ToolOutcome,
    ) -> hive_runtime::chat::RepoFuture<'a, ()> {
        Box::pin(async move {
            let turn = turn_id.to_owned();
            let intent = intent.clone();
            self.submit(move |c| execution::finish_tool(c, &turn, &intent, outcome))
                .await
                .map_err(|e| Box::new(e) as hive_runtime::chat::RepoError)
        })
    }
    fn claim_next(
        &self,
    ) -> hive_runtime::chat::RepoFuture<'_, Option<hive_runtime::chat::TurnWork>> {
        Box::pin(async move {
            if !self.shared.workspace_healthy.load(Ordering::Acquire) {
                return Err(Box::new(StoreError::Unavailable) as hive_runtime::chat::RepoError);
            }
            self.claim_next_turn()
                .await
                .map_err(|error| Box::new(error) as hive_runtime::chat::RepoError)
        })
    }
    fn reconcile(&self) -> hive_runtime::chat::RepoFuture<'_, ()> {
        Box::pin(async move {
            self.submit(|connection| {
                let tx = connection
                    .unchecked_transaction()
                    .map_err(|_| StoreError::Database)?;
                children::wake(&tx)?;
                tx.commit().map_err(|_| StoreError::Database)
            })
            .await
            .map_err(|error| Box::new(error) as hive_runtime::chat::RepoError)
        })
    }
    fn checkpoint_visible<'a>(
        &'a self,
        turn_id: &'a str,
        text: &'a str,
    ) -> hive_runtime::chat::RepoFuture<'a, ()> {
        Box::pin(async move {
            self.checkpoint_visible(turn_id, text)
                .await
                .map_err(|error| Box::new(error) as hive_runtime::chat::RepoError)
        })
    }
    fn finish_turn<'a>(
        &'a self,
        turn_id: &'a str,
        outcome: hive_runtime::chat::ChatOutcome,
    ) -> hive_runtime::chat::RepoFuture<'a, ()> {
        Box::pin(async move {
            self.finish_chat_turn(turn_id, outcome)
                .await
                .map_err(|error| Box::new(error) as hive_runtime::chat::RepoError)
        })
    }
    fn cancellation_requested<'a>(
        &'a self,
        turn_id: &'a str,
    ) -> hive_runtime::chat::RepoFuture<'a, bool> {
        Box::pin(async move {
            self.turn_cancellation_requested(turn_id)
                .await
                .map_err(|error| Box::new(error) as hive_runtime::chat::RepoError)
        })
    }
    fn turn_control<'a>(
        &'a self,
        turn_id: &'a str,
    ) -> hive_runtime::chat::RepoFuture<'a, hive_runtime::chat::TurnControl> {
        Box::pin(async move {
            self.turn_control(turn_id)
                .await
                .map_err(|error| Box::new(error) as hive_runtime::chat::RepoError)
        })
    }

    fn retain_provider_snapshot<'a>(
        &'a self,
        turn_id: &'a str,
    ) -> hive_runtime::chat::RepoFuture<'a, bool> {
        Box::pin(async move {
            self.retain_provider_snapshot(turn_id)
                .await
                .map_err(|error| Box::new(error) as hive_runtime::chat::RepoError)
        })
    }
}
