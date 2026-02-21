#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct FocusCardEventContext {
    latest_needs_input_prompt: Option<String>,
    latest_blocked_reason: Option<String>,
    latest_checkpoint_summary: Option<String>,
}

const FOCUS_CARD_EVENT_SCAN_LIMIT: usize = 512;
const FOCUS_CARD_ARTIFACT_LIMIT: usize = 6;
const FOCUS_CARD_TEXT_MAX_CHARS: usize = 220;

impl FocusCardEventContext {
    fn is_complete(&self) -> bool {
        self.latest_needs_input_prompt.is_some()
            && self.latest_blocked_reason.is_some()
            && self.latest_checkpoint_summary.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FocusCardDetails {
    why_attention: String,
    recommended_response: String,
    evidence_lines: Vec<String>,
}

fn project_focus_card_pane(
    inbox_item_id: &InboxItemId,
    inbox_rows: &[UiInboxRow],
    domain: &ProjectionState,
) -> CenterPaneState {
    if let Some(item) = inbox_rows
        .iter()
        .find(|row| &row.inbox_item_id == inbox_item_id)
    {
        let workflow = item
            .workflow_state
            .as_ref()
            .map(|state| format!("{state:?}"))
            .unwrap_or_else(|| "Unknown".to_owned());
        let session = match (&item.session_id, &item.session_status) {
            (Some(session_id), status) => {
                session_display_with_status(domain, session_id, status.as_ref())
            }
            (None, _) => "None".to_owned(),
        };
        let focus_details = build_focus_card_details(item, domain);
        let mut lines = vec![
            format!("Title: {}", item.title),
            format!("Kind: {:?}", item.kind),
            format!("Work item: {}", item.work_item_id.as_str()),
            format!("Workflow: {workflow}"),
            format!("Session: {session}"),
            String::new(),
            "Why attention is required:".to_owned(),
            format!("- {}", focus_details.why_attention),
            String::new(),
            "Recommended response:".to_owned(),
            format!("- {}", focus_details.recommended_response),
            String::new(),
            "Evidence:".to_owned(),
        ];
        if focus_details.evidence_lines.is_empty() {
            lines.push("- No evidence links or artifacts yet.".to_owned());
        } else {
            lines.extend(
                focus_details
                    .evidence_lines
                    .iter()
                    .map(|line| format!("- {line}")),
            );
        }
        lines.push(String::new());
        lines.push("Shortcuts: v d diff | v t tests | v p PR | v c chat".to_owned());
        CenterPaneState {
            title: format!("Focus Card {}", inbox_item_id.as_str()),
            lines,
        }
    } else {
        CenterPaneState {
            title: format!("Focus Card {}", inbox_item_id.as_str()),
            lines: vec!["Selected inbox item is not available.".to_owned()],
        }
    }
}

fn build_focus_card_details(item: &UiInboxRow, domain: &ProjectionState) -> FocusCardDetails {
    let event_context =
        collect_focus_card_event_context(domain, &item.work_item_id, item.session_id.as_ref());
    let evidence_lines = collect_focus_card_evidence_lines(domain, item, &event_context);

    let mut why_attention = match item.kind {
        InboxItemKind::NeedsDecision => {
            "The worker needs a decision before it can continue implementation.".to_owned()
        }
        InboxItemKind::Blocked => {
            "The worker reported a blocker and cannot self-progress.".to_owned()
        }
        InboxItemKind::NeedsApproval => {
            "The workflow hit a human-approval gate before the next transition.".to_owned()
        }
        InboxItemKind::ReadyForReview => {
            "The work item is prepared for review and final direction.".to_owned()
        }
        InboxItemKind::FYI => {
            "This is a progress digest and does not require an immediate interrupt.".to_owned()
        }
    };

    if matches!(
        item.session_status,
        Some(WorkerSessionStatus::WaitingForUser)
    ) {
        why_attention.push_str(" Session status is WaitingForUser.");
    } else if matches!(item.session_status, Some(WorkerSessionStatus::Blocked)) {
        why_attention.push_str(" Session status is Blocked.");
    }

    let recommended_response = match item.kind {
        InboxItemKind::NeedsDecision => {
            if let Some(prompt) = event_context.latest_needs_input_prompt.as_deref() {
                format!("Answer the worker prompt to unblock progress: {prompt}")
            } else {
                "Provide a clear decision and continue the session.".to_owned()
            }
        }
        InboxItemKind::Blocked => {
            if let Some(reason) = event_context.latest_blocked_reason.as_deref() {
                format!("Address the blocker and resume the session: {reason}")
            } else {
                "Review the latest logs/artifacts, resolve the blocker, then resume.".to_owned()
            }
        }
        InboxItemKind::NeedsApproval => {
            "Review the linked artifacts (PR/tests/diff) and approve the next workflow gate if ready."
                .to_owned()
        }
        InboxItemKind::ReadyForReview => {
            "Review the latest evidence and leave review feedback or advance the item.".to_owned()
        }
        InboxItemKind::FYI => "No immediate action required; review when you batch FYI updates.".to_owned(),
    };

    FocusCardDetails {
        why_attention,
        recommended_response,
        evidence_lines,
    }
}

fn collect_focus_card_event_context(
    domain: &ProjectionState,
    work_item_id: &WorkItemId,
    session_id: Option<&WorkerSessionId>,
) -> FocusCardEventContext {
    let mut context = FocusCardEventContext::default();
    let mut matched_event_count = 0usize;

    for event in domain.events.iter().rev() {
        let matches_focus_context = match session_id {
            Some(session_id) => {
                event.session_id.as_ref() == Some(session_id)
                    || (event.work_item_id.as_ref() == Some(work_item_id)
                        && event.session_id.is_none())
            }
            None => event.work_item_id.as_ref() == Some(work_item_id),
        };
        if !matches_focus_context {
            continue;
        }
        matched_event_count += 1;

        match &event.payload {
            OrchestrationEventPayload::SessionNeedsInput(payload)
                if context.latest_needs_input_prompt.is_none() =>
            {
                context.latest_needs_input_prompt = Some(compact_focus_card_text(&payload.prompt));
            }
            OrchestrationEventPayload::SessionBlocked(payload)
                if context.latest_blocked_reason.is_none() =>
            {
                context.latest_blocked_reason = Some(compact_focus_card_text(&payload.reason));
            }
            OrchestrationEventPayload::SessionCheckpoint(payload)
                if context.latest_checkpoint_summary.is_none() =>
            {
                context.latest_checkpoint_summary = Some(compact_focus_card_text(&payload.summary));
            }
            _ => {}
        }

        if context.is_complete() || matched_event_count >= FOCUS_CARD_EVENT_SCAN_LIMIT {
            break;
        }
    }

    context
}

fn compact_focus_card_text(raw: &str) -> String {
    let compact = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= FOCUS_CARD_TEXT_MAX_CHARS {
        return compact;
    }

    let keep = FOCUS_CARD_TEXT_MAX_CHARS.saturating_sub(3);
    let truncated = compact.chars().take(keep).collect::<String>();
    format!("{truncated}...")
}

fn collect_focus_card_evidence_lines(
    domain: &ProjectionState,
    item: &UiInboxRow,
    event_context: &FocusCardEventContext,
) -> Vec<String> {
    let mut lines = Vec::new();

    if let Some(prompt) = event_context.latest_needs_input_prompt.as_deref() {
        lines.push(format!("Latest input request: {prompt}"));
    }
    if let Some(reason) = event_context.latest_blocked_reason.as_deref() {
        lines.push(format!("Latest blocker reason: {reason}"));
    }
    if let Some(summary) = event_context.latest_checkpoint_summary.as_deref() {
        lines.push(format!("Latest checkpoint summary: {summary}"));
    }

    if let Some(work_item) = domain.work_items.get(&item.work_item_id) {
        let mut seen_artifact_ids = HashSet::new();
        let mut shown_artifacts = 0usize;
        let mut omitted_artifacts = 0usize;

        for artifact_id in work_item.artifacts.iter().rev() {
            if !seen_artifact_ids.insert(artifact_id.clone()) {
                continue;
            }
            let Some(artifact) = domain.artifacts.get(artifact_id) else {
                continue;
            };

            if shown_artifacts < FOCUS_CARD_ARTIFACT_LIMIT {
                lines.push(format_artifact_evidence_line(artifact));
                shown_artifacts += 1;
            } else {
                omitted_artifacts += 1;
            }
        }

        if omitted_artifacts > 0 {
            lines.push(format!("{omitted_artifacts} older artifacts not shown."));
        }
    }

    lines
}

fn format_artifact_evidence_line(artifact: &ArtifactProjection) -> String {
    format!(
        "{:?} artifact '{}' -> {}",
        artifact.kind, artifact.label, artifact.uri
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WhichKeyHint {
    key: KeyStroke,
    description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WhichKeyOverlayState {
    prefix: Vec<KeyStroke>,
    group_label: Option<String>,
    hints: Vec<WhichKeyHint>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TicketStatusGroup {
    status: String,
    tickets: Vec<TicketSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TicketProjectGroup {
    project: String,
    collapsed: bool,
    status_groups: Vec<TicketStatusGroup>,
}

impl TicketProjectGroup {
    fn ticket_count(&self) -> usize {
        self.status_groups
            .iter()
            .map(|status_group| status_group.tickets.len())
            .sum()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TicketPickerRowRef {
    ProjectHeader {
        project_index: usize,
    },
    Ticket {
        project_index: usize,
        status_index: usize,
        ticket_index: usize,
    },
}

#[derive(Clone)]
struct TicketPickerOverlayState {
    visible: bool,
    loading: bool,
    starting_ticket_id: Option<TicketId>,
    archiving_ticket_id: Option<TicketId>,
    creating: bool,
    new_ticket_mode: bool,
    new_ticket_brief_editor: EditorState,
    new_ticket_brief_event_handler: EditorEventHandler,
    error: Option<String>,
    project_groups: Vec<TicketProjectGroup>,
    ticket_rows: Vec<TicketPickerRowRef>,
    selected_row_index: Option<usize>,
    repository_prompt_ticket: Option<TicketSummary>,
    repository_prompt_project_id: Option<String>,
    repository_prompt_input: InputState,
    repository_prompt_missing_mapping: bool,
    archive_confirm_ticket: Option<TicketSummary>,
    ticket_profile_overrides: HashMap<TicketId, String>,
}

impl Default for TicketPickerOverlayState {
    fn default() -> Self {
        Self {
            visible: false,
            loading: false,
            starting_ticket_id: None,
            archiving_ticket_id: None,
            creating: false,
            new_ticket_mode: false,
            new_ticket_brief_editor: insert_mode_editor_state(),
            new_ticket_brief_event_handler: EditorEventHandler::default(),
            error: None,
            project_groups: Vec::new(),
            ticket_rows: Vec::new(),
            selected_row_index: None,
            repository_prompt_ticket: None,
            repository_prompt_project_id: None,
            repository_prompt_input: InputState::empty(),
            repository_prompt_missing_mapping: false,
            archive_confirm_ticket: None,
            ticket_profile_overrides: HashMap::new(),
        }
    }
}

impl TicketPickerOverlayState {
    fn selected_row(&self) -> Option<&TicketPickerRowRef> {
        self.selected_row_index
            .and_then(|index| self.ticket_rows.get(index))
    }

    fn selected_project_index(&self) -> Option<usize> {
        match self.selected_row()? {
            TicketPickerRowRef::ProjectHeader { project_index } => Some(*project_index),
            TicketPickerRowRef::Ticket { project_index, .. } => Some(*project_index),
        }
    }

    fn selected_project_name(&self) -> Option<String> {
        let project_index = self.selected_project_index()?;
        self.project_groups
            .get(project_index)
            .map(|project_group| project_group.project.clone())
    }

    fn selected_ticket(&self) -> Option<&TicketSummary> {
        let row = self.selected_row()?;
        let TicketPickerRowRef::Ticket {
            project_index,
            status_index,
            ticket_index,
        } = row
        else {
            return None;
        };
        self.project_groups
            .get(*project_index)
            .and_then(|project_group| project_group.status_groups.get(*status_index))
            .and_then(|status_group| status_group.tickets.get(*ticket_index))
    }

    fn project_names(&self) -> Vec<String> {
        self.project_groups
            .iter()
            .map(|project_group| project_group.project.clone())
            .collect()
    }

    fn tickets_snapshot(&self) -> Vec<TicketSummary> {
        self.project_groups
            .iter()
            .flat_map(|project_group| {
                project_group
                    .status_groups
                    .iter()
                    .flat_map(|status_group| status_group.tickets.iter().cloned())
            })
            .collect()
    }

    fn open(&mut self) {
        self.visible = true;
        self.loading = true;
        self.archiving_ticket_id = None;
        self.creating = false;
        self.new_ticket_mode = false;
        clear_editor_state(&mut self.new_ticket_brief_editor);
        self.error = None;
        self.repository_prompt_ticket = None;
        self.repository_prompt_project_id = None;
        self.repository_prompt_input.clear();
        self.repository_prompt_missing_mapping = true;
        self.archive_confirm_ticket = None;
    }

    fn close(&mut self) {
        self.visible = false;
        self.loading = false;
        self.starting_ticket_id = None;
        self.archiving_ticket_id = None;
        self.creating = false;
        self.new_ticket_mode = false;
        clear_editor_state(&mut self.new_ticket_brief_editor);
        self.error = None;
        self.repository_prompt_ticket = None;
        self.repository_prompt_project_id = None;
        self.repository_prompt_input.clear();
        self.repository_prompt_missing_mapping = true;
        self.archive_confirm_ticket = None;
    }

    fn ticket_override_for(&self, ticket_id: &TicketId) -> Option<&str> {
        self.ticket_profile_overrides
            .get(ticket_id)
            .map(String::as_str)
    }

    fn has_repository_prompt(&self) -> bool {
        self.repository_prompt_ticket.is_some()
    }

    fn start_repository_prompt(
        &mut self,
        ticket: TicketSummary,
        project_id: String,
        repository_path_hint: Option<String>,
    ) {
        self.repository_prompt_ticket = Some(ticket);
        self.repository_prompt_project_id = Some(project_id);
        if let Some(repository_path_hint) = repository_path_hint {
            self.repository_prompt_input.set_text(repository_path_hint);
            self.repository_prompt_missing_mapping = false;
        } else {
            self.repository_prompt_input.clear();
            self.repository_prompt_missing_mapping = true;
        }
    }

    fn cancel_repository_prompt(&mut self) {
        self.repository_prompt_ticket = None;
        self.repository_prompt_project_id = None;
        self.repository_prompt_input.clear();
        self.repository_prompt_missing_mapping = true;
    }

    fn begin_new_ticket_mode(&mut self) {
        if self.creating {
            return;
        }
        self.new_ticket_mode = true;
        self.error = None;
    }

    fn cancel_new_ticket_mode(&mut self) {
        self.new_ticket_mode = false;
        clear_editor_state(&mut self.new_ticket_brief_editor);
        self.creating = false;
    }

    fn can_submit_new_ticket(&self) -> bool {
        !self.creating && !editor_state_text(&self.new_ticket_brief_editor).trim().is_empty()
    }

    fn apply_tickets(
        &mut self,
        tickets: Vec<TicketSummary>,
        project_names: Vec<String>,
        priority_states: &[String],
    ) {
        self.apply_tickets_preferring(tickets, project_names, priority_states, None);
    }

    fn apply_tickets_preferring(
        &mut self,
        tickets: Vec<TicketSummary>,
        project_names: Vec<String>,
        priority_states: &[String],
        preferred_ticket_id: Option<&TicketId>,
    ) {
        let selected_project_index = self.selected_project_index();
        let selected_ticket_id = preferred_ticket_id
            .cloned()
            .or_else(|| self.selected_ticket().map(|ticket| ticket.ticket_id.clone()));
        let collapsed_projects = self
            .project_groups
            .iter()
            .filter(|project_group| project_group.collapsed)
            .map(|project_group| normalize_ticket_project(project_group.project.as_str()))
            .collect::<HashSet<_>>();
        self.project_groups =
            group_tickets_by_project(
                tickets,
                project_names,
                priority_states,
                &collapsed_projects,
            );
        self.rebuild_ticket_rows(selected_ticket_id.as_ref(), selected_project_index);
    }

    fn move_selection(&mut self, delta: isize) {
        if self.ticket_rows.is_empty() {
            self.selected_row_index = None;
            return;
        }

        let current = self.selected_row_index.unwrap_or(0) as isize;
        let upper = self.ticket_rows.len() as isize - 1;
        self.selected_row_index = Some((current + delta).clamp(0, upper) as usize);
    }

    fn fold_selected_project(&mut self) {
        self.set_selected_project_collapsed(true);
    }

    fn unfold_selected_project(&mut self) {
        self.set_selected_project_collapsed(false);
    }

    fn set_selected_project_collapsed(&mut self, collapsed: bool) {
        let Some(project_index) = self.selected_project_index() else {
            return;
        };
        let Some(project_group) = self.project_groups.get_mut(project_index) else {
            return;
        };
        if project_group.collapsed == collapsed {
            return;
        }
        project_group.collapsed = collapsed;
        self.rebuild_ticket_rows(None, Some(project_index));
    }

    fn rebuild_ticket_rows(
        &mut self,
        preferred_ticket_id: Option<&TicketId>,
        preferred_project_index: Option<usize>,
    ) {
        self.ticket_rows =
            self.project_groups
                .iter()
                .enumerate()
                .flat_map(|(project_index, project_group)| {
                    let mut rows = vec![TicketPickerRowRef::ProjectHeader { project_index }];
                    if !project_group.collapsed {
                        rows.extend(project_group.status_groups.iter().enumerate().flat_map(
                            move |(status_index, status_group)| {
                                status_group.tickets.iter().enumerate().map(
                                    move |(ticket_index, _)| TicketPickerRowRef::Ticket {
                                        project_index,
                                        status_index,
                                        ticket_index,
                                    },
                                )
                            },
                        ));
                    }
                    rows
                })
                .collect();

        if self.ticket_rows.is_empty() {
            self.selected_row_index = None;
            return;
        }

        if let Some(preferred_ticket_id) = preferred_ticket_id {
            if let Some(index) = self.ticket_rows.iter().position(|row| {
                let TicketPickerRowRef::Ticket {
                    project_index,
                    status_index,
                    ticket_index,
                } = row
                else {
                    return false;
                };
                self.project_groups
                    .get(*project_index)
                    .and_then(|project_group| project_group.status_groups.get(*status_index))
                    .and_then(|status_group| status_group.tickets.get(*ticket_index))
                    .map(|ticket| ticket.ticket_id == *preferred_ticket_id)
                    .unwrap_or_default()
            }) {
                self.selected_row_index = Some(index);
                return;
            }
        }

        if let Some(preferred_project_index) = preferred_project_index {
            if let Some(index) = self.ticket_rows.iter().position(|row| {
                matches!(
                    row,
                    TicketPickerRowRef::ProjectHeader { project_index }
                        if *project_index == preferred_project_index
                )
            }) {
                self.selected_row_index = Some(index);
                return;
            }
        }

        self.selected_row_index = Some(0);
    }
}

#[derive(Debug, Clone)]
struct WorkflowProfilesModalState {
    visible: bool,
    saving: bool,
    profiles: Vec<WorkflowInteractionProfile>,
    default_profile: String,
    selected_profile_index: usize,
    selected_state_index: usize,
    renaming: bool,
    rename_input: InputState,
    error: Option<String>,
}

impl Default for WorkflowProfilesModalState {
    fn default() -> Self {
        Self {
            visible: false,
            saving: false,
            profiles: Vec::new(),
            default_profile: String::new(),
            selected_profile_index: 0,
            selected_state_index: 0,
            renaming: false,
            rename_input: InputState::empty(),
            error: None,
        }
    }
}

impl WorkflowProfilesModalState {
    fn open(&mut self, config: WorkflowInteractionProfilesConfig) {
        self.visible = true;
        self.saving = false;
        self.renaming = false;
        self.rename_input.clear();
        self.error = None;
        self.profiles = config.profiles;
        self.default_profile = config.default_profile;
        if self.profiles.is_empty() {
            let fallback = WorkflowInteractionProfilesConfig::default();
            self.profiles = fallback.profiles;
            self.default_profile = fallback.default_profile;
        }
        if self.selected_profile_index >= self.profiles.len() {
            self.selected_profile_index = 0;
        }
        self.selected_state_index = self.selected_state_index.min(
            self.selected_profile()
                .map(|profile| profile.levels.len().saturating_sub(1))
                .unwrap_or(0),
        );
    }

    fn close(&mut self) {
        self.visible = false;
        self.saving = false;
        self.renaming = false;
        self.rename_input.clear();
        self.error = None;
    }

    fn selected_profile(&self) -> Option<&WorkflowInteractionProfile> {
        self.profiles.get(self.selected_profile_index)
    }

    fn selected_profile_mut(&mut self) -> Option<&mut WorkflowInteractionProfile> {
        self.profiles.get_mut(self.selected_profile_index)
    }

    fn cycle_profile(&mut self, delta: isize) {
        if self.profiles.is_empty() {
            return;
        }
        let len = self.profiles.len() as isize;
        let current = self.selected_profile_index as isize;
        let next = (current + delta).rem_euclid(len) as usize;
        self.selected_profile_index = next;
        self.selected_state_index = 0;
        self.error = None;
    }

    fn move_state_selection(&mut self, delta: isize) {
        let Some(profile) = self.selected_profile() else {
            return;
        };
        if profile.levels.is_empty() {
            self.selected_state_index = 0;
            return;
        }
        let len = profile.levels.len() as isize;
        let current = self.selected_state_index as isize;
        let next = (current + delta).rem_euclid(len) as usize;
        self.selected_state_index = next;
    }

    fn toggle_selected_state_level(&mut self) {
        let selected_state_index = self.selected_state_index;
        let Some(profile) = self.selected_profile_mut() else {
            return;
        };
        let Some(level) = profile.levels.get_mut(selected_state_index) else {
            return;
        };
        level.level = match level.level {
            WorkflowInteractionLevel::Manual => WorkflowInteractionLevel::Auto,
            WorkflowInteractionLevel::Auto => WorkflowInteractionLevel::Manual,
        };
        self.error = None;
    }

    fn add_profile(&mut self) {
        let seed_levels = self
            .selected_profile()
            .map(|profile| profile.levels.clone())
            .unwrap_or_else(|| {
                orchestrator_core::all_workflow_states()
                    .into_iter()
                    .map(|state| WorkflowInteractionStateLevel {
                        state,
                        level: WorkflowInteractionLevel::Manual,
                    })
                    .collect()
            });

        let mut next_index = self.profiles.len() + 1;
        let name = loop {
            let candidate = format!("profile-{next_index}");
            if !self
                .profiles
                .iter()
                .any(|profile| profile.name.eq_ignore_ascii_case(candidate.as_str()))
            {
                break candidate;
            }
            next_index += 1;
        };

        self.profiles.push(WorkflowInteractionProfile {
            name: name.clone(),
            levels: seed_levels,
        });
        self.selected_profile_index = self.profiles.len().saturating_sub(1);
        self.selected_state_index = 0;
        self.default_profile = self.default_profile.trim().to_owned();
        if self.default_profile.is_empty() {
            self.default_profile = name;
        }
        self.error = None;
    }

    fn delete_selected_profile(&mut self) {
        if self.profiles.len() <= 1 {
            self.error = Some("at least one profile is required".to_owned());
            return;
        }
        if self.selected_profile_index >= self.profiles.len() {
            self.selected_profile_index = 0;
        }
        let removed = self.profiles.remove(self.selected_profile_index);
        if self.default_profile == removed.name {
            self.default_profile = self
                .profiles
                .first()
                .map(|profile| profile.name.clone())
                .unwrap_or_default();
        }
        if self.selected_profile_index >= self.profiles.len() {
            self.selected_profile_index = self.profiles.len().saturating_sub(1);
        }
        self.selected_state_index = 0;
        self.error = None;
    }

    fn set_selected_default(&mut self) {
        if let Some(profile) = self.selected_profile() {
            self.default_profile = profile.name.clone();
            self.error = None;
        }
    }

    fn begin_rename_selected_profile(&mut self) {
        let Some(profile) = self.selected_profile() else {
            return;
        };
        self.rename_input.set_text(profile.name.clone());
        self.renaming = true;
        self.error = None;
    }

    fn cancel_rename(&mut self) {
        self.renaming = false;
        self.rename_input.clear();
    }

    fn submit_rename(&mut self) {
        let next_name = self.rename_input.text().trim().to_owned();
        if next_name.is_empty() {
            self.error = Some("profile name cannot be empty".to_owned());
            return;
        }
        let selected_index = self.selected_profile_index;
        if self.profiles.iter().enumerate().any(|(index, profile)| {
            index != selected_index && profile.name.eq_ignore_ascii_case(next_name.as_str())
        }) {
            self.error = Some("profile name already exists".to_owned());
            return;
        }

        let previous_name = self
            .profiles
            .get(selected_index)
            .map(|profile| profile.name.clone())
            .unwrap_or_default();
        if let Some(profile) = self.profiles.get_mut(selected_index) {
            profile.name = next_name.clone();
        }
        if self.default_profile == previous_name {
            self.default_profile = next_name;
        }
        self.renaming = false;
        self.rename_input.clear();
        self.error = None;
    }

    fn as_config(&self) -> WorkflowInteractionProfilesConfig {
        WorkflowInteractionProfilesConfig {
            default_profile: self.default_profile.clone(),
            profiles: self.profiles.clone(),
        }
    }
}

#[derive(Debug, Clone)]
enum TicketPickerEvent {
    TicketsLoaded {
        tickets: Vec<TicketSummary>,
        projects: Vec<String>,
        profile_overrides: HashMap<TicketId, String>,
    },
    TicketsLoadFailed {
        message: String,
    },
    SessionWorkflowAdvanced {
        outcome: SessionWorkflowAdvanceOutcome,
    },
    SessionWorkflowAdvanceFailed {
        session_id: WorkerSessionId,
        message: String,
    },
    TicketStarted {
        started_session_id: WorkerSessionId,
        projection: Option<ProjectionState>,
        tickets: Option<Vec<TicketSummary>>,
        profile_overrides: Option<HashMap<TicketId, String>>,
        warning: Option<String>,
    },
    TicketStartFailed {
        message: String,
    },
    TicketArchived {
        archived_ticket: TicketSummary,
        tickets: Option<Vec<TicketSummary>>,
        warning: Option<String>,
    },
    TicketArchiveFailed {
        ticket: TicketSummary,
        message: String,
        tickets: Option<Vec<TicketSummary>>,
    },
    TicketCreated {
        created_ticket: TicketSummary,
        submit_mode: TicketCreateSubmitMode,
        tickets: Option<Vec<TicketSummary>>,
        warning: Option<String>,
    },
    TicketCreateFailed {
        message: String,
        tickets: Option<Vec<TicketSummary>>,
        warning: Option<String>,
    },
    TicketStartRequiresRepository {
        ticket: TicketSummary,
        project_id: String,
        repository_path_hint: Option<String>,
        message: String,
    },
    TicketProfileOverrideUpdated {
        ticket_id: TicketId,
        profile_name: Option<String>,
    },
    TicketProfileOverrideUpdateFailed {
        ticket_id: TicketId,
        message: String,
    },
    WorkflowProfilesSaved {
        config: WorkflowInteractionProfilesConfig,
    },
    WorkflowProfilesSaveFailed {
        message: String,
    },
    SessionDiffLoaded {
        diff: SessionWorktreeDiff,
    },
    SessionDiffFailed {
        session_id: WorkerSessionId,
        message: String,
    },
    SessionArchived {
        session_id: WorkerSessionId,
        warning: Option<String>,
        event: StoredEventEnvelope,
    },
    SessionArchiveFailed {
        session_id: WorkerSessionId,
        message: String,
    },
    InboxItemPublished {
        event: StoredEventEnvelope,
    },
    InboxItemPublishFailed {
        message: String,
    },
    InboxItemResolved {
        event: Option<StoredEventEnvelope>,
    },
    InboxItemResolveFailed {
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorktreeDiffModalState {
    session_id: WorkerSessionId,
    base_branch: String,
    content: String,
    loading: bool,
    error: Option<String>,
    scroll: u16,
    cursor_line: usize,
    selected_file_index: usize,
    selected_hunk_index: usize,
    focus: DiffPaneFocus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiffPaneFocus {
    Files,
    Diff,
}

#[derive(Debug, Clone)]
struct MergeQueueRequest {
    session_id: WorkerSessionId,
    _context: SupervisorCommandContext,
    kind: MergeQueueCommandKind,
}
