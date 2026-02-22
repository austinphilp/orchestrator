pub struct AppFrontendController<S: Supervisor, G: GithubClient> {
    app: std::sync::Arc<App<S, G>>,
    worker_backend: Option<std::sync::Arc<dyn WorkerBackend + Send + Sync>>,
    snapshot: std::sync::Arc<tokio::sync::RwLock<FrontendSnapshot>>,
    event_tx: tokio::sync::broadcast::Sender<FrontendEvent>,
    lifecycle: tokio::sync::Mutex<Option<AppFrontendControllerLifecycle>>,
}

struct AppFrontendControllerLifecycle {
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl<S: Supervisor, G: GithubClient> AppFrontendController<S, G> {
    pub fn from_startup_state(
        app: std::sync::Arc<App<S, G>>,
        worker_backend: Option<std::sync::Arc<dyn WorkerBackend + Send + Sync>>,
        state: &StartupState,
    ) -> Self {
        Self::new(
            app,
            worker_backend,
            FrontendSnapshot {
                status: state.status.clone(),
                projection: state.projection.clone(),
                application_mode: FrontendApplicationMode::Manual,
            },
        )
    }

    pub fn new(
        app: std::sync::Arc<App<S, G>>,
        worker_backend: Option<std::sync::Arc<dyn WorkerBackend + Send + Sync>>,
        initial_snapshot: FrontendSnapshot,
    ) -> Self {
        let (event_tx, _) = tokio::sync::broadcast::channel(256);
        Self {
            app,
            worker_backend,
            snapshot: std::sync::Arc::new(tokio::sync::RwLock::new(initial_snapshot)),
            event_tx,
            lifecycle: tokio::sync::Mutex::new(None),
        }
    }

    pub async fn start(&self) -> Result<(), CoreError>
    where
        S: Supervisor + Send + Sync + 'static,
        G: GithubClient + Send + Sync + 'static,
    {
        let mut lifecycle = self.lifecycle.lock().await;
        if lifecycle.is_some() {
            return Ok(());
        }

        let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
        let app = self.app.clone();
        let worker_backend = self.worker_backend.clone();
        let snapshot = self.snapshot.clone();
        let event_tx = self.event_tx.clone();
        let poll_interval = std::time::Duration::from_secs(
            app.config
                .runtime
                .pr_pipeline_poll_interval_secs
                .max(1),
        );

        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(poll_interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut terminal_streamed_sessions = std::collections::HashSet::new();
            loop {
                tokio::select! {
                    _ = shutdown_rx.changed() => {
                        break;
                    }
                    _ = ticker.tick() => {
                        let projection_result = {
                            let app = app.clone();
                            tokio::task::spawn_blocking(move || app.projection_state()).await
                        };

                        let projection = match projection_result {
                            Ok(Ok(projection)) => projection,
                            Ok(Err(error)) => {
                                let _ = event_tx.send(FrontendEvent::Notification(FrontendNotification {
                                    level: FrontendNotificationLevel::Warning,
                                    message: format!("frontend projection refresh failed: {error}"),
                                    work_item_id: None,
                                    session_id: None,
                                }));
                                continue;
                            }
                            Err(error) => {
                                let _ = event_tx.send(FrontendEvent::Notification(FrontendNotification {
                                    level: FrontendNotificationLevel::Warning,
                                    message: format!("frontend projection refresh task failed: {error}"),
                                    work_item_id: None,
                                    session_id: None,
                                }));
                                continue;
                            }
                        };

                        let mut guard = snapshot.write().await;
                        if guard.projection == projection {
                            drop(guard);
                            ensure_terminal_streams(
                                worker_backend.as_ref(),
                                &event_tx,
                                &mut shutdown_rx,
                                &projection,
                                &mut terminal_streamed_sessions,
                            );
                            continue;
                        }
                        guard.projection = projection;
                        let _ = event_tx.send(FrontendEvent::SnapshotUpdated(guard.clone()));
                        let projection_for_streams = guard.projection.clone();
                        drop(guard);
                        ensure_terminal_streams(
                            worker_backend.as_ref(),
                            &event_tx,
                            &mut shutdown_rx,
                            &projection_for_streams,
                            &mut terminal_streamed_sessions,
                        );
                    }
                }
            }
        });

        *lifecycle = Some(AppFrontendControllerLifecycle { shutdown_tx, task });
        Ok(())
    }

    pub async fn stop(&self) -> Result<(), CoreError> {
        let lifecycle = {
            let mut guard = self.lifecycle.lock().await;
            guard.take()
        };

        if let Some(lifecycle) = lifecycle {
            let _ = lifecycle.shutdown_tx.send(true);
            let _ = lifecycle.task.await;
        }
        Ok(())
    }

    async fn submit_terminal_input_intent(
        &self,
        session_id: &WorkerSessionId,
        input: &str,
    ) -> Result<(), CoreError> {
        let Some(worker_backend) = self.worker_backend.clone() else {
            return Err(CoreError::DependencyUnavailable(
                "frontend terminal input unavailable: worker backend is not configured".to_owned(),
            ));
        };

        let mut payload = input.to_owned();
        if !payload.ends_with('\n') {
            payload.push('\n');
        }
        let handle = SessionHandle {
            session_id: RuntimeSessionId::from(session_id.clone()),
            backend: worker_backend.kind(),
        };
        worker_backend
            .send_input(&handle, payload.as_bytes())
            .await
            .map_err(CoreError::Runtime)?;
        Ok(())
    }

    async fn submit_needs_input_response_intent(
        &self,
        session_id: &WorkerSessionId,
        prompt_id: &str,
        answers: &[orchestrator_domain::BackendNeedsInputAnswer],
    ) -> Result<(), CoreError> {
        let Some(worker_backend) = self.worker_backend.clone() else {
            return Err(CoreError::DependencyUnavailable(
                "frontend needs-input response unavailable: worker backend is not configured"
                    .to_owned(),
            ));
        };
        let handle = SessionHandle {
            session_id: RuntimeSessionId::from(session_id.clone()),
            backend: worker_backend.kind(),
        };
        worker_backend
            .respond_to_needs_input(&handle, prompt_id, answers)
            .await
            .map_err(CoreError::Runtime)?;
        Ok(())
    }
}

fn ensure_terminal_streams(
    worker_backend: Option<&std::sync::Arc<dyn WorkerBackend + Send + Sync>>,
    event_tx: &tokio::sync::broadcast::Sender<FrontendEvent>,
    shutdown_rx: &mut tokio::sync::watch::Receiver<bool>,
    projection: &ProjectionState,
    streamed_sessions: &mut std::collections::HashSet<WorkerSessionId>,
) {
    let Some(worker_backend) = worker_backend.cloned() else {
        return;
    };

    for (session_id, session) in &projection.sessions {
        if matches!(
            session.status,
            Some(WorkerSessionStatus::Done | WorkerSessionStatus::Crashed)
        ) {
            continue;
        }
        if !streamed_sessions.insert(session_id.clone()) {
            continue;
        }

        let stream_session_id = session_id.clone();
        let backend_kind = worker_backend.kind();
        let handle = SessionHandle {
            session_id: RuntimeSessionId::from(stream_session_id.clone()),
            backend: backend_kind,
        };
        let worker_backend_for_task = worker_backend.clone();
        let mut stream_shutdown_rx = shutdown_rx.clone();
        let stream_event_tx = event_tx.clone();
        tokio::spawn(async move {
            let mut stream = match worker_backend_for_task.subscribe(&handle).await {
                Ok(stream) => stream,
                Err(error) => {
                    let _ = stream_event_tx.send(FrontendEvent::TerminalSession(
                        FrontendTerminalEvent::StreamFailed {
                            session_id: stream_session_id,
                            message: error.to_string(),
                            is_session_not_found: matches!(
                                error,
                                orchestrator_domain::RuntimeError::SessionNotFound(_)
                            ),
                        },
                    ));
                    return;
                }
            };

            loop {
                tokio::select! {
                    _ = stream_shutdown_rx.changed() => return,
                    next = stream.next_event() => {
                        match next {
                            Ok(Some(orchestrator_domain::BackendEvent::Output(output))) => {
                                let _ = stream_event_tx.send(FrontendEvent::TerminalSession(
                                    FrontendTerminalEvent::Output {
                                        session_id: stream_session_id.clone(),
                                        output,
                                    }
                                ));
                            }
                            Ok(Some(orchestrator_domain::BackendEvent::TurnState(turn_state))) => {
                                let _ = stream_event_tx.send(FrontendEvent::TerminalSession(
                                    FrontendTerminalEvent::TurnState {
                                        session_id: stream_session_id.clone(),
                                        turn_state,
                                    }
                                ));
                            }
                            Ok(Some(orchestrator_domain::BackendEvent::NeedsInput(needs_input))) => {
                                let _ = stream_event_tx.send(FrontendEvent::TerminalSession(
                                    FrontendTerminalEvent::NeedsInput {
                                        session_id: stream_session_id.clone(),
                                        needs_input,
                                    }
                                ));
                            }
                            Ok(Some(_)) => {}
                            Ok(None) => {
                                let _ = stream_event_tx.send(FrontendEvent::TerminalSession(
                                    FrontendTerminalEvent::StreamEnded {
                                        session_id: stream_session_id.clone(),
                                    },
                                ));
                                return;
                            }
                            Err(error) => {
                                let _ = stream_event_tx.send(FrontendEvent::TerminalSession(
                                    FrontendTerminalEvent::StreamFailed {
                                        session_id: stream_session_id.clone(),
                                        message: error.to_string(),
                                        is_session_not_found: matches!(
                                            error,
                                            orchestrator_domain::RuntimeError::SessionNotFound(_)
                                        ),
                                    },
                                ));
                                return;
                            }
                        }
                    }
                }
            }
        });
    }
}

fn apply_intent_to_snapshot(snapshot: &mut FrontendSnapshot, intent: &FrontendIntent) -> bool {
    match intent {
        FrontendIntent::Command(command) => match command {
            FrontendCommandIntent::SetApplicationModeAutopilot => {
                if snapshot.application_mode != FrontendApplicationMode::Autopilot {
                    snapshot.application_mode = FrontendApplicationMode::Autopilot;
                    true
                } else {
                    false
                }
            }
            FrontendCommandIntent::SetApplicationModeManual => {
                if snapshot.application_mode != FrontendApplicationMode::Manual {
                    snapshot.application_mode = FrontendApplicationMode::Manual;
                    true
                } else {
                    false
                }
            }
            _ => false,
        },
        FrontendIntent::SendTerminalInput { .. }
        | FrontendIntent::RespondToNeedsInput { .. } => false,
    }
}

struct FrontendBroadcastSubscription {
    receiver: tokio::sync::broadcast::Receiver<FrontendEvent>,
}

struct CoreFrontendSubscriptionAdapter {
    inner: FrontendEventStream,
}

#[async_trait::async_trait]
impl FrontendEventSubscription for FrontendBroadcastSubscription {
    async fn next_event(&mut self) -> Result<Option<FrontendEvent>, CoreError> {
        loop {
            match self.receiver.recv().await {
                Ok(event) => return Ok(Some(event)),
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(None),
            }
        }
    }
}

#[async_trait::async_trait]
impl orchestrator_domain::FrontendEventSubscription for CoreFrontendSubscriptionAdapter {
    async fn next_event(&mut self) -> Result<Option<orchestrator_domain::FrontendEvent>, CoreError> {
        Ok(self.inner.next_event().await?.map(Into::into))
    }
}

#[async_trait::async_trait]
impl<S, G> FrontendController for AppFrontendController<S, G>
where
    S: Supervisor + Send + Sync + 'static,
    G: GithubClient + Send + Sync + 'static,
{
    async fn snapshot(&self) -> Result<FrontendSnapshot, CoreError> {
        Ok(self.snapshot.read().await.clone())
    }

    async fn submit_intent(&self, intent: FrontendIntent) -> Result<(), CoreError> {
        match &intent {
            FrontendIntent::Command(_) => {
                let mut guard = self.snapshot.write().await;
                if apply_intent_to_snapshot(&mut guard, &intent) {
                    let _ = self.event_tx.send(FrontendEvent::SnapshotUpdated(guard.clone()));
                }
            }
            FrontendIntent::SendTerminalInput { session_id, input } => {
                self.submit_terminal_input_intent(session_id, input.as_str())
                    .await?;
            }
            FrontendIntent::RespondToNeedsInput {
                session_id,
                prompt_id,
                answers,
            } => {
                self.submit_needs_input_response_intent(
                    session_id,
                    prompt_id.as_str(),
                    answers.as_slice(),
                )
                .await?;
            }
        }
        Ok(())
    }

    async fn subscribe(&self) -> Result<FrontendEventStream, CoreError> {
        Ok(Box::new(FrontendBroadcastSubscription {
            receiver: self.event_tx.subscribe(),
        }))
    }
}

#[async_trait::async_trait]
impl<S, G> orchestrator_domain::FrontendController for AppFrontendController<S, G>
where
    S: Supervisor + Send + Sync + 'static,
    G: GithubClient + Send + Sync + 'static,
{
    async fn snapshot(&self) -> Result<orchestrator_domain::FrontendSnapshot, CoreError> {
        Ok(<Self as FrontendController>::snapshot(self).await?.into())
    }

    async fn submit_intent(&self, intent: orchestrator_domain::FrontendIntent) -> Result<(), CoreError> {
        <Self as FrontendController>::submit_intent(self, intent.into()).await
    }

    async fn subscribe(&self) -> Result<orchestrator_domain::FrontendEventStream, CoreError> {
        let stream = <Self as FrontendController>::subscribe(self).await?;
        Ok(Box::new(CoreFrontendSubscriptionAdapter { inner: stream }))
    }
}

#[cfg(test)]
mod frontend_controller_tests {
    use super::*;

    #[test]
    fn apply_intent_updates_application_mode() {
        let mut snapshot = FrontendSnapshot {
            status: "ready".to_owned(),
            projection: ProjectionState::default(),
            application_mode: FrontendApplicationMode::Manual,
        };

        let changed = apply_intent_to_snapshot(
            &mut snapshot,
            &FrontendIntent::Command(FrontendCommandIntent::SetApplicationModeAutopilot),
        );
        assert!(changed);
        assert_eq!(snapshot.application_mode, FrontendApplicationMode::Autopilot);

        let changed = apply_intent_to_snapshot(
            &mut snapshot,
            &FrontendIntent::Command(FrontendCommandIntent::SetApplicationModeAutopilot),
        );
        assert!(!changed);
        assert_eq!(snapshot.application_mode, FrontendApplicationMode::Autopilot);

        let changed = apply_intent_to_snapshot(
            &mut snapshot,
            &FrontendIntent::Command(FrontendCommandIntent::SetApplicationModeManual),
        );
        assert!(changed);
        assert_eq!(snapshot.application_mode, FrontendApplicationMode::Manual);
    }

}
