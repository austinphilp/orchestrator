use orchestrator_domain::{BackendEvent, BackendNeedsInputAnswer, RuntimeError};

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
            app.config.runtime.pr_pipeline_poll_interval_secs.max(1),
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
        answers: &[BackendNeedsInputAnswer],
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
                            is_session_not_found: matches!(error, RuntimeError::SessionNotFound(_)),
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
                            Ok(Some(BackendEvent::Output(output))) => {
                                let _ = stream_event_tx.send(FrontendEvent::TerminalSession(
                                    FrontendTerminalEvent::Output {
                                        session_id: stream_session_id.clone(),
                                        output,
                                    }
                                ));
                            }
                            Ok(Some(BackendEvent::TurnState(turn_state))) => {
                                let _ = stream_event_tx.send(FrontendEvent::TerminalSession(
                                    FrontendTerminalEvent::TurnState {
                                        session_id: stream_session_id.clone(),
                                        turn_state,
                                    }
                                ));
                            }
                            Ok(Some(BackendEvent::NeedsInput(needs_input))) => {
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
                                            RuntimeError::SessionNotFound(_)
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
        FrontendIntent::Command(command) => apply_command_intent_to_snapshot(snapshot, *command),
        FrontendIntent::SendTerminalInput { .. } | FrontendIntent::RespondToNeedsInput { .. } => {
            false
        }
    }
}

fn apply_command_intent_to_snapshot(
    snapshot: &mut FrontendSnapshot,
    command: FrontendCommandIntent,
) -> bool {
    match command {
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
        FrontendCommandIntent::EnterNormalMode
        | FrontendCommandIntent::EnterInsertMode
        | FrontendCommandIntent::ToggleGlobalSupervisorChat
        | FrontendCommandIntent::OpenTerminalForSelected
        | FrontendCommandIntent::OpenDiffInspectorForSelected
        | FrontendCommandIntent::OpenTestInspectorForSelected
        | FrontendCommandIntent::OpenPrInspectorForSelected
        | FrontendCommandIntent::OpenChatInspectorForSelected
        | FrontendCommandIntent::StartTerminalEscapeChord
        | FrontendCommandIntent::QuitShell
        | FrontendCommandIntent::FocusNextInbox
        | FrontendCommandIntent::FocusPreviousInbox
        | FrontendCommandIntent::CycleBatchNext
        | FrontendCommandIntent::CycleBatchPrevious
        | FrontendCommandIntent::JumpFirstInbox
        | FrontendCommandIntent::JumpLastInbox
        | FrontendCommandIntent::JumpBatchDecideOrUnblock
        | FrontendCommandIntent::JumpBatchApprovals
        | FrontendCommandIntent::JumpBatchReviewReady
        | FrontendCommandIntent::JumpBatchFyiDigest
        | FrontendCommandIntent::OpenTicketPicker
        | FrontendCommandIntent::CloseTicketPicker
        | FrontendCommandIntent::TicketPickerMoveNext
        | FrontendCommandIntent::TicketPickerMovePrevious
        | FrontendCommandIntent::TicketPickerFoldProject
        | FrontendCommandIntent::TicketPickerUnfoldProject
        | FrontendCommandIntent::TicketPickerStartSelected
        | FrontendCommandIntent::ToggleWorktreeDiffModal
        | FrontendCommandIntent::AdvanceTerminalWorkflowStage
        | FrontendCommandIntent::ArchiveSelectedSession
        | FrontendCommandIntent::OpenSessionOutputForSelectedInbox => false,
    }
}

fn command_intent_notification(
    command: FrontendCommandIntent,
    snapshot_changed: bool,
) -> Option<FrontendNotification> {
    match command {
        FrontendCommandIntent::SetApplicationModeAutopilot => {
            if snapshot_changed {
                None
            } else {
                Some(command_info_notification(
                    command,
                    "application mode is already autopilot",
                ))
            }
        }
        FrontendCommandIntent::SetApplicationModeManual => {
            if snapshot_changed {
                None
            } else {
                Some(command_info_notification(
                    command,
                    "application mode is already manual",
                ))
            }
        }
        FrontendCommandIntent::TicketPickerStartSelected => Some(command_warning_notification(
            command,
            "requires selected ticket context from the ticket picker; FrontendIntent::Command carries no ticket payload",
        )),
        FrontendCommandIntent::AdvanceTerminalWorkflowStage
        | FrontendCommandIntent::ArchiveSelectedSession => {
            Some(command_warning_notification(
                command,
                "requires selected session/inbox context; FrontendIntent::Command carries no UI selection payload",
            ))
        }
        FrontendCommandIntent::EnterNormalMode
        | FrontendCommandIntent::EnterInsertMode
        | FrontendCommandIntent::ToggleGlobalSupervisorChat
        | FrontendCommandIntent::OpenTerminalForSelected
        | FrontendCommandIntent::OpenDiffInspectorForSelected
        | FrontendCommandIntent::OpenTestInspectorForSelected
        | FrontendCommandIntent::OpenPrInspectorForSelected
        | FrontendCommandIntent::OpenChatInspectorForSelected
        | FrontendCommandIntent::StartTerminalEscapeChord
        | FrontendCommandIntent::QuitShell
        | FrontendCommandIntent::FocusNextInbox
        | FrontendCommandIntent::FocusPreviousInbox
        | FrontendCommandIntent::CycleBatchNext
        | FrontendCommandIntent::CycleBatchPrevious
        | FrontendCommandIntent::JumpFirstInbox
        | FrontendCommandIntent::JumpLastInbox
        | FrontendCommandIntent::JumpBatchDecideOrUnblock
        | FrontendCommandIntent::JumpBatchApprovals
        | FrontendCommandIntent::JumpBatchReviewReady
        | FrontendCommandIntent::JumpBatchFyiDigest
        | FrontendCommandIntent::OpenTicketPicker
        | FrontendCommandIntent::CloseTicketPicker
        | FrontendCommandIntent::TicketPickerMoveNext
        | FrontendCommandIntent::TicketPickerMovePrevious
        | FrontendCommandIntent::TicketPickerFoldProject
        | FrontendCommandIntent::TicketPickerUnfoldProject
        | FrontendCommandIntent::ToggleWorktreeDiffModal
        | FrontendCommandIntent::OpenSessionOutputForSelectedInbox => None,
    }
}

fn command_info_notification(command: FrontendCommandIntent, detail: &str) -> FrontendNotification {
    FrontendNotification {
        level: FrontendNotificationLevel::Info,
        message: format!("{}: {detail}", command.command_id()),
        work_item_id: None,
        session_id: None,
    }
}

fn command_warning_notification(
    command: FrontendCommandIntent,
    detail: &str,
) -> FrontendNotification {
    FrontendNotification {
        level: FrontendNotificationLevel::Warning,
        message: format!("{}: {detail}", command.command_id()),
        work_item_id: None,
        session_id: None,
    }
}

struct FrontendBroadcastSubscription {
    receiver: tokio::sync::broadcast::Receiver<FrontendEvent>,
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
            FrontendIntent::Command(command) => {
                let (snapshot_update, notification) = {
                    let mut guard = self.snapshot.write().await;
                    let snapshot_changed = apply_intent_to_snapshot(&mut guard, &intent);
                    let snapshot_update =
                        snapshot_changed.then(|| FrontendEvent::SnapshotUpdated(guard.clone()));
                    let notification = command_intent_notification(*command, snapshot_changed)
                        .map(FrontendEvent::Notification);
                    (snapshot_update, notification)
                };

                if let Some(event) = snapshot_update {
                    let _ = self.event_tx.send(event);
                }
                if let Some(event) = notification {
                    let _ = self.event_tx.send(event);
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

#[cfg(test)]
mod frontend_controller_tests {
    use super::*;
    use orchestrator_ticketing::{self, AddTicketCommentRequest, UpdateTicketStateRequest};
    use std::sync::Arc;

    struct FrontendControllerTestSupervisor;

    #[async_trait::async_trait]
    impl Supervisor for FrontendControllerTestSupervisor {
        async fn health_check(&self) -> Result<(), CoreError> {
            Ok(())
        }
    }

    struct FrontendControllerTestGithub;

    #[async_trait::async_trait]
    impl GithubClient for FrontendControllerTestGithub {
        async fn health_check(&self) -> Result<(), orchestrator_vcs_repos::CoreError> {
            Ok(())
        }
    }

    struct FrontendControllerTestTicketingProvider;

    #[async_trait::async_trait]
    impl orchestrator_ticketing::TicketingProvider for FrontendControllerTestTicketingProvider {
        fn provider(&self) -> orchestrator_ticketing::TicketProvider {
            orchestrator_ticketing::TicketProvider::Linear
        }

        async fn health_check(&self) -> Result<(), orchestrator_ticketing::CoreError> {
            Ok(())
        }

        async fn list_tickets(
            &self,
            _query: orchestrator_ticketing::TicketQuery,
        ) -> Result<Vec<orchestrator_ticketing::TicketSummary>, orchestrator_ticketing::CoreError>
        {
            Ok(Vec::new())
        }

        async fn create_ticket(
            &self,
            _request: orchestrator_ticketing::CreateTicketRequest,
        ) -> Result<orchestrator_ticketing::TicketSummary, orchestrator_ticketing::CoreError>
        {
            Err(orchestrator_ticketing::CoreError::DependencyUnavailable(
                "create_ticket is not required for frontend controller command-intent tests"
                    .to_owned(),
            ))
        }

        async fn update_ticket_state(
            &self,
            _request: UpdateTicketStateRequest,
        ) -> Result<(), orchestrator_ticketing::CoreError> {
            Ok(())
        }

        async fn add_comment(
            &self,
            _request: AddTicketCommentRequest,
        ) -> Result<(), orchestrator_ticketing::CoreError> {
            Ok(())
        }
    }

    fn test_frontend_controller(
    ) -> AppFrontendController<FrontendControllerTestSupervisor, FrontendControllerTestGithub> {
        let app = Arc::new(App {
            config: AppConfig::default(),
            ticketing: Arc::new(FrontendControllerTestTicketingProvider),
            supervisor: FrontendControllerTestSupervisor,
            github: FrontendControllerTestGithub,
        });
        AppFrontendController::new(
            app,
            None,
            FrontendSnapshot {
                status: "ready".to_owned(),
                projection: ProjectionState::default(),
                application_mode: FrontendApplicationMode::Manual,
            },
        )
    }

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
        assert_eq!(
            snapshot.application_mode,
            FrontendApplicationMode::Autopilot
        );

        let changed = apply_intent_to_snapshot(
            &mut snapshot,
            &FrontendIntent::Command(FrontendCommandIntent::SetApplicationModeAutopilot),
        );
        assert!(!changed);
        let notification =
            command_intent_notification(FrontendCommandIntent::SetApplicationModeAutopilot, changed)
                .expect("mode no-op should emit deterministic notification");
        assert_eq!(notification.level, FrontendNotificationLevel::Info);
        assert!(notification.message.contains("ui.app_mode.autopilot"));
        assert_eq!(
            snapshot.application_mode,
            FrontendApplicationMode::Autopilot
        );

        let changed = apply_intent_to_snapshot(
            &mut snapshot,
            &FrontendIntent::Command(FrontendCommandIntent::SetApplicationModeManual),
        );
        assert!(changed);
        assert_eq!(snapshot.application_mode, FrontendApplicationMode::Manual);
    }

    #[test]
    fn command_intent_notification_skips_ui_shell_commands() {
        for command in [
            FrontendCommandIntent::OpenTicketPicker,
            FrontendCommandIntent::CloseTicketPicker,
            FrontendCommandIntent::ToggleWorktreeDiffModal,
            FrontendCommandIntent::OpenSessionOutputForSelectedInbox,
        ] {
            assert!(
                command_intent_notification(command, false).is_none(),
                "expected no controller notification for {}",
                command.command_id()
            );
        }
    }

    #[tokio::test]
    async fn submit_intent_skips_notifications_for_ui_shell_commands() {
        let controller = test_frontend_controller();
        let mut subscription = controller.subscribe().await.expect("subscribe");
        controller
            .submit_intent(FrontendIntent::Command(
                FrontendCommandIntent::OpenTicketPicker,
            ))
            .await
            .expect("submit command intent");

        let no_event = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            subscription.next_event(),
        )
        .await;
        assert!(no_event.is_err(), "expected no frontend event for UI-shell command");
    }

    #[tokio::test]
    async fn submit_intent_emits_ticket_action_warning_notification() {
        let controller = test_frontend_controller();
        let mut subscription = controller.subscribe().await.expect("subscribe");
        controller
            .submit_intent(FrontendIntent::Command(
                FrontendCommandIntent::TicketPickerStartSelected,
            ))
            .await
            .expect("submit command intent");

        let event = subscription
            .next_event()
            .await
            .expect("subscription next event result")
            .expect("subscription event");
        let FrontendEvent::Notification(notification) = event else {
            panic!("expected notification event");
        };
        assert_eq!(notification.level, FrontendNotificationLevel::Warning);
        assert!(notification.message.contains("ui.ticket_picker.start_selected"));
        assert!(notification.message.contains("requires selected ticket context"));
    }

    #[tokio::test]
    async fn submit_intent_emits_workflow_action_warning_notification() {
        let controller = test_frontend_controller();
        let mut subscription = controller.subscribe().await.expect("subscribe");
        controller
            .submit_intent(FrontendIntent::Command(
                FrontendCommandIntent::AdvanceTerminalWorkflowStage,
            ))
            .await
            .expect("submit command intent");

        let event = subscription
            .next_event()
            .await
            .expect("subscription next event result")
            .expect("subscription event");
        let FrontendEvent::Notification(notification) = event else {
            panic!("expected notification event");
        };
        assert_eq!(notification.level, FrontendNotificationLevel::Warning);
        assert!(notification.message.contains("ui.terminal.workflow.advance"));
        assert!(notification
            .message
            .contains("requires selected session/inbox context"));
    }
}
