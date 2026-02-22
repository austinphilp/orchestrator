fn project_artifact_inspector_pane(
    inspector: ArtifactInspectorKind,
    work_item_id: &WorkItemId,
    domain: &ProjectionState,
) -> CenterPaneState {
    let mut lines = vec![format!("Work item: {}", work_item_id.as_str())];
    match inspector {
        ArtifactInspectorKind::Diff => {
            lines.extend(project_diff_inspector_lines(work_item_id, domain));
        }
        ArtifactInspectorKind::Test => {
            lines.extend(project_test_inspector_lines(work_item_id, domain));
        }
        ArtifactInspectorKind::PullRequest => {
            lines.extend(project_pr_inspector_lines(work_item_id, domain));
        }
        ArtifactInspectorKind::Chat => {
            lines.extend(project_chat_inspector_lines(work_item_id, domain));
        }
    }
    CenterPaneState {
        title: format!("{} {}", inspector.pane_title(), work_item_id.as_str()),
        lines,
    }
}

fn project_diff_inspector_lines(
    work_item_id: &WorkItemId,
    domain: &ProjectionState,
) -> Vec<String> {
    let mut lines = vec!["Diff artifacts:".to_owned()];
    let artifacts =
        collect_work_item_artifacts(work_item_id, domain, INSPECTOR_ARTIFACT_LIMIT, |artifact| {
            artifact.kind == ArtifactKind::Diff
        });

    if artifacts.is_empty() {
        lines.push("- No diff artifacts available yet.".to_owned());
        return lines;
    }

    for artifact in artifacts {
        lines.push(format!("- {} -> {}", artifact.label, artifact.uri));
        if let Some(diffstat) = diffstat_summary_line(artifact) {
            lines.push(format!("  {diffstat}"));
        }
    }

    lines
}

fn project_test_inspector_lines(
    work_item_id: &WorkItemId,
    domain: &ProjectionState,
) -> Vec<String> {
    let mut lines = vec!["Test artifacts:".to_owned()];
    let artifacts = collect_work_item_artifacts(
        work_item_id,
        domain,
        INSPECTOR_ARTIFACT_LIMIT,
        is_test_artifact,
    );

    if artifacts.is_empty() {
        lines.push("- No test artifacts available yet.".to_owned());
    } else {
        for artifact in artifacts {
            lines.push(format!("- {} -> {}", artifact.label, artifact.uri));
            if let Some(tail) = test_tail_summary_line(artifact) {
                lines.push(format!("  {tail}"));
            }
        }
    }

    if let Some(summary) = latest_checkpoint_summary_for_work_item(work_item_id, domain) {
        lines.push(format!("Latest checkpoint summary: {summary}"));
    }
    if let Some(reason) = latest_blocked_reason_for_work_item(work_item_id, domain) {
        lines.push(format!("Latest blocker reason: {reason}"));
    }

    lines
}

fn project_pr_inspector_lines(work_item_id: &WorkItemId, domain: &ProjectionState) -> Vec<String> {
    let mut lines = vec!["PR artifacts:".to_owned()];
    let artifacts = collect_work_item_artifacts(
        work_item_id,
        domain,
        INSPECTOR_ARTIFACT_LIMIT,
        is_pr_artifact,
    );

    if artifacts.is_empty() {
        lines.push("- No PR artifacts available yet.".to_owned());
        return lines;
    }

    for artifact in artifacts {
        lines.push(format!("- {} -> {}", artifact.label, artifact.uri));
        if let Some(metadata) = pr_metadata_summary_line(artifact) {
            lines.push(format!("  {metadata}"));
        }
    }

    lines
}

fn project_chat_inspector_lines(
    work_item_id: &WorkItemId,
    domain: &ProjectionState,
) -> Vec<String> {
    let mut lines = vec!["Supervisor output:".to_owned()];
    let event_lines = collect_chat_event_lines(work_item_id, domain);
    if event_lines.is_empty() {
        lines.push("- No supervisor transcript events captured yet.".to_owned());
    } else {
        lines.extend(event_lines.into_iter().map(|line| format!("- {line}")));
    }
    if let Some(summary) = latest_supervisor_query_metrics_line(work_item_id, domain) {
        lines.push(format!("- {summary}"));
    }

    lines.push(String::new());
    lines.push("Chat artifacts:".to_owned());
    let artifacts = collect_work_item_artifacts(
        work_item_id,
        domain,
        INSPECTOR_ARTIFACT_LIMIT,
        is_chat_artifact,
    );
    if artifacts.is_empty() {
        lines.push("- No chat artifacts available yet.".to_owned());
    } else {
        lines.extend(
            artifacts
                .iter()
                .map(|artifact| format!("- {} -> {}", artifact.label, artifact.uri)),
        );
    }

    lines
}

fn collect_work_item_artifacts<'a, F>(
    work_item_id: &WorkItemId,
    domain: &'a ProjectionState,
    limit: usize,
    predicate: F,
) -> Vec<&'a ArtifactProjection>
where
    F: Fn(&ArtifactProjection) -> bool,
{
    let Some(work_item) = domain.work_items.get(work_item_id) else {
        return Vec::new();
    };

    let mut seen = HashSet::new();
    let mut artifacts = Vec::new();
    for artifact_id in work_item.artifacts.iter().rev() {
        if !seen.insert(artifact_id.clone()) {
            continue;
        }
        let Some(artifact) = domain.artifacts.get(artifact_id) else {
            continue;
        };
        if artifact.work_item_id == *work_item_id && predicate(artifact) {
            artifacts.push(artifact);
            if artifacts.len() >= limit {
                break;
            }
        }
    }

    artifacts
}

fn diffstat_summary_line(artifact: &ArtifactProjection) -> Option<String> {
    let files = first_uri_param(artifact.uri.as_str(), &["files", "changed"]);
    let additions = first_uri_param(artifact.uri.as_str(), &["insertions", "added", "adds"]);
    let deletions = first_uri_param(artifact.uri.as_str(), &["deletions", "removed", "dels"]);

    if files.is_some() || additions.is_some() || deletions.is_some() {
        return Some(format!(
            "Diffstat: {} files changed, +{}/-{}",
            files.as_deref().unwrap_or("unknown"),
            additions.as_deref().unwrap_or("0"),
            deletions.as_deref().unwrap_or("0")
        ));
    }

    let label = compact_focus_card_text(artifact.label.as_str());
    if looks_like_diffstat(label.as_str()) {
        return Some(format!("Diffstat: {label}"));
    }

    None
}

fn test_tail_summary_line(artifact: &ArtifactProjection) -> Option<String> {
    let tail = first_uri_param(artifact.uri.as_str(), &["tail", "snippet", "summary"])?;
    Some(format!(
        "Latest test tail: {}",
        compact_focus_card_text(tail.as_str())
    ))
}

fn pr_metadata_summary_line(artifact: &ArtifactProjection) -> Option<String> {
    let uri = artifact.uri.as_str();
    let pull_index = uri.find("/pull/")?;
    let after_pull = &uri[pull_index + "/pull/".len()..];
    let pr_number = after_pull
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    if pr_number.is_empty() {
        return None;
    }

    let repo_segment = uri
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(uri)
        .split('?')
        .next()
        .unwrap_or(uri);
    let draft = first_uri_param(uri, &["draft"]);
    let draft_suffix = if draft.as_deref().is_some_and(is_truthy) {
        " (draft)"
    } else {
        ""
    };

    Some(format!(
        "PR metadata: #{pr_number}{draft_suffix} from {repo_segment}"
    ))
}

fn first_uri_param(uri: &str, keys: &[&str]) -> Option<String> {
    let (_, query) = uri.split_once('?')?;
    for pair in query.split('&') {
        let (raw_key, raw_value) = pair.split_once('=').unwrap_or((pair, ""));
        if keys.iter().any(|key| raw_key.eq_ignore_ascii_case(key)) {
            return Some(decode_query_component(raw_value));
        }
    }
    None
}

fn decode_query_component(raw: &str) -> String {
    let mut decoded = Vec::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => decoded.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                let hi = decode_hex_nibble(bytes[index + 1]);
                let lo = decode_hex_nibble(bytes[index + 2]);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    decoded.push((hi << 4) | lo);
                    index += 3;
                    continue;
                }
                decoded.push(bytes[index]);
            }
            byte => decoded.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(decoded.as_slice()).into_owned()
}

fn decode_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn is_truthy(value: &str) -> bool {
    value == "1" || value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes")
}

fn looks_like_diffstat(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("files changed")
        || (lower.contains("insertion") && lower.contains("deletion"))
        || (lower.contains('+') && lower.contains('-') && lower.contains("file"))
}

fn is_test_artifact(artifact: &ArtifactProjection) -> bool {
    if artifact.kind == ArtifactKind::TestRun {
        return true;
    }
    if artifact.kind != ArtifactKind::LogSnippet && artifact.kind != ArtifactKind::Link {
        return false;
    }

    contains_any_case_insensitive(
        artifact.label.as_str(),
        &["test", "pytest", "cargo test", "failing", "junit"],
    ) || contains_any_case_insensitive(artifact.uri.as_str(), &["test", "junit", "report", "tail"])
}

fn is_pr_artifact(artifact: &ArtifactProjection) -> bool {
    artifact.kind == ArtifactKind::PR
        || (artifact.kind == ArtifactKind::Link
            && contains_any_case_insensitive(
                artifact.uri.as_str(),
                &["/pull/", "pullrequest", "pull-request"],
            ))
}

fn is_chat_artifact(artifact: &ArtifactProjection) -> bool {
    if artifact.kind == ArtifactKind::Export {
        return true;
    }

    contains_any_case_insensitive(artifact.label.as_str(), &["supervisor", "chat", "response"])
        || contains_any_case_insensitive(
            artifact.uri.as_str(),
            &["supervisor", "chat", "conversation", "assistant"],
        )
}

fn contains_any_case_insensitive(haystack: &str, needles: &[&str]) -> bool {
    let haystack_lower = haystack.to_ascii_lowercase();
    needles
        .iter()
        .any(|needle| haystack_lower.contains(&needle.to_ascii_lowercase()))
}

fn collect_chat_event_lines(work_item_id: &WorkItemId, domain: &ProjectionState) -> Vec<String> {
    let mut lines = Vec::new();
    for event in domain.events.iter().rev().take(INSPECTOR_EVENT_SCAN_LIMIT) {
        if event.work_item_id.as_ref() != Some(work_item_id) {
            continue;
        }

        let message = match &event.payload {
            OrchestrationEventPayload::SessionNeedsInput(payload) => Some(format!(
                "worker: {}",
                compact_focus_card_text(payload.prompt.as_str())
            )),
            OrchestrationEventPayload::UserResponded(payload) => Some(format!(
                "you: {}",
                compact_focus_card_text(payload.message.as_str())
            )),
            OrchestrationEventPayload::SessionCheckpoint(payload) => Some(format!(
                "worker checkpoint: {}",
                compact_focus_card_text(payload.summary.as_str())
            )),
            OrchestrationEventPayload::SessionBlocked(payload) => Some(format!(
                "worker blocked: {}",
                compact_focus_card_text(payload.reason.as_str())
            )),
            OrchestrationEventPayload::SupervisorQueryStarted(payload) => {
                let descriptor = payload
                    .template
                    .as_deref()
                    .map(|template| format!("template={template}"))
                    .or_else(|| payload.query.as_deref().map(|_| "freeform".to_owned()))
                    .unwrap_or_else(|| "unknown".to_owned());
                Some(format!(
                    "supervisor query started: {descriptor} ({})",
                    payload.query_id
                ))
            }
            OrchestrationEventPayload::SupervisorQueryCancelled(payload) => Some(format!(
                "supervisor query cancel requested: {:?} ({})",
                payload.source, payload.query_id
            )),
            OrchestrationEventPayload::SupervisorQueryFinished(payload) => Some(format!(
                "supervisor query finished {:?}: {}ms, {} chunks, {} chars",
                payload.finish_reason,
                payload.duration_ms,
                payload.chunk_count,
                payload.output_chars
            )),
            _ => None,
        };

        if let Some(message) = message {
            lines.push(format!("{} | {message}", event.occurred_at));
        }
        if lines.len() >= INSPECTOR_CHAT_EVENT_LIMIT {
            break;
        }
    }

    lines.reverse();
    lines
}

fn latest_supervisor_query_metrics_line(
    work_item_id: &WorkItemId,
    domain: &ProjectionState,
) -> Option<String> {
    for event in domain.events.iter().rev().take(INSPECTOR_EVENT_SCAN_LIMIT) {
        if event.work_item_id.as_ref() != Some(work_item_id) {
            continue;
        }

        let OrchestrationEventPayload::SupervisorQueryFinished(payload) = &event.payload else {
            continue;
        };

        let usage = payload
            .usage
            .as_ref()
            .map(|usage| {
                format!(
                    " usage(input={} output={} total={})",
                    usage.input_tokens, usage.output_tokens, usage.total_tokens
                )
            })
            .unwrap_or_default();
        let cancellation = payload
            .cancellation_source
            .as_ref()
            .map(|source| format!(" cancellation={source:?}"))
            .unwrap_or_default();

        return Some(format!(
            "Latest query metrics: id={} reason={:?} duration={}ms chunks={} chars={}{}{}",
            payload.query_id,
            payload.finish_reason,
            payload.duration_ms,
            payload.chunk_count,
            payload.output_chars,
            usage,
            cancellation
        ));
    }

    None
}

#[allow(dead_code)]
fn build_supervisor_chat_request(
    selected_row: &UiInboxRow,
    domain: &ProjectionState,
    supervisor_model: &str,
) -> LlmChatRequest {
    let workflow = selected_row
        .workflow_state
        .as_ref()
        .map(|state| format!("{state:?}"))
        .unwrap_or_else(|| "Unknown".to_owned());
    let session = match (&selected_row.session_id, &selected_row.session_status) {
        (Some(session_id), Some(status)) => format!("{} ({status:?})", session_id.as_str()),
        (Some(session_id), None) => format!("{} (Unknown)", session_id.as_str()),
        (None, _) => "None".to_owned(),
    };
    let chat_event_lines = collect_chat_event_lines(&selected_row.work_item_id, domain);
    let mut prompt_lines = vec![
        "You are answering for the orchestrator supervisor chat pane.".to_owned(),
        "Use only the supplied context. If uncertain, say Unknown.".to_owned(),
        String::new(),
        format!("Work item: {}", selected_row.work_item_id.as_str()),
        format!("Inbox title: {}", selected_row.title),
        format!("Inbox kind: {:?}", selected_row.kind),
        format!("Workflow: {workflow}"),
        format!("Session: {session}"),
        String::new(),
        "Recent transcript events:".to_owned(),
    ];
    if chat_event_lines.is_empty() {
        prompt_lines.push("- none".to_owned());
    } else {
        prompt_lines.extend(chat_event_lines.into_iter().map(|line| format!("- {line}")));
    }
    prompt_lines.push(String::new());
    prompt_lines.push("Respond with:".to_owned());
    prompt_lines.push("- Current activity (1-2 bullets)".to_owned());
    prompt_lines.push("- What needs me now (ordered bullets)".to_owned());
    prompt_lines.push("- Recommended response to send to worker (single message)".to_owned());

    LlmChatRequest {
        model: supervisor_model.to_owned(),
        tools: Vec::new(),
        messages: vec![
            LlmMessage {
                role: LlmRole::System,
                content: "You are the orchestrator supervisor. Keep responses terse, operational, and grounded in provided context."
                    .to_owned(),
                name: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
            LlmMessage {
                role: LlmRole::User,
                content: prompt_lines.join("\n"),
                name: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
        ],
        temperature: Some(0.2),
        tool_choice: None,
        max_output_tokens: Some(700),
    }
}

#[allow(dead_code)]
fn build_global_supervisor_chat_request(query: &str, supervisor_model: &str) -> LlmChatRequest {
    LlmChatRequest {
        model: supervisor_model.to_owned(),
        tools: Vec::new(),
        messages: vec![
            LlmMessage {
                role: LlmRole::System,
                content: "You are the orchestrator supervisor. Answer concisely and operationally. If information is missing, say Unknown."
                    .to_owned(),
                name: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
            LlmMessage {
                role: LlmRole::User,
                content: query.to_owned(),
                name: None,
                tool_calls: Vec::new(),
                tool_call_id: None,
            },
        ],
        temperature: Some(0.2),
        tool_choice: None,
        max_output_tokens: Some(700),
    }
}

fn classify_supervisor_stream_error(message: &str) -> SupervisorResponseState {
    let lower = message.to_ascii_lowercase();
    if lower.contains("429") || lower.contains("rate limit") {
        SupervisorResponseState::RateLimited
    } else if lower.contains("missing selected item")
        || lower.contains("select an inbox item")
        || lower.contains("non-empty question")
        || lower.contains("malformed context")
    {
        SupervisorResponseState::NoContext
    } else if lower.contains("401")
        || lower.contains("403")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("auth")
        || lower.contains("api key")
    {
        SupervisorResponseState::AuthUnavailable
    } else if lower.contains("network")
        || lower.contains("timeout")
        || lower.contains("connect")
        || lower.contains("transport")
        || lower.contains("dns")
        || lower.contains("tls")
        || lower.contains("no llm provider")
        || lower.contains("tokio runtime")
        || lower.contains("dependency unavailable")
    {
        SupervisorResponseState::BackendUnavailable
    } else {
        SupervisorResponseState::BackendUnavailable
    }
}

fn response_state_warning_label(state: SupervisorResponseState) -> &'static str {
    match state {
        SupervisorResponseState::Nominal => "supervisor",
        SupervisorResponseState::NoContext => "no-context",
        SupervisorResponseState::AuthUnavailable => "auth",
        SupervisorResponseState::BackendUnavailable => "backend",
        SupervisorResponseState::RateLimited => "rate-limit",
        SupervisorResponseState::HighCost => "high-cost",
    }
}

fn supervisor_state_message(state: SupervisorResponseState) -> &'static str {
    match state {
        SupervisorResponseState::Nominal => "Supervisor stream is healthy.",
        SupervisorResponseState::NoContext => {
            "No usable context was available for this supervisor request."
        }
        SupervisorResponseState::AuthUnavailable => {
            "Supervisor authentication failed. Check configured API credentials."
        }
        SupervisorResponseState::BackendUnavailable => {
            "Supervisor backend is currently unavailable."
        }
        SupervisorResponseState::RateLimited => "Supervisor is rate-limited. Retry after cooldown.",
        SupervisorResponseState::HighCost => {
            "Supervisor response cost is high. Use a tighter scope or narrower questions."
        }
    }
}

fn parse_rate_limit_cooldown_hint(message: &str) -> Option<String> {
    let lowered = message.to_ascii_lowercase();
    if let Some(index) = lowered.find("rate limit reset at ") {
        let suffix = &message[index + "rate limit reset at ".len()..];
        let reset_at = suffix
            .split('.')
            .next()
            .unwrap_or_default()
            .trim()
            .trim_matches('`');
        if !reset_at.is_empty() {
            return Some(format!("rate limit reset at {reset_at}"));
        }
    }

    if let Some(index) = lowered.find("retry after ") {
        let suffix = &message[index + "retry after ".len()..];
        let cooldown = suffix
            .split('.')
            .next()
            .unwrap_or_default()
            .trim()
            .trim_matches('`');
        if !cooldown.is_empty() {
            return Some(format!("retry after {cooldown}"));
        }
    }
    None
}

fn usage_trips_high_cost_state(usage: &LlmTokenUsage) -> bool {
    usage.total_tokens >= SUPERVISOR_STREAM_HIGH_COST_TOTAL_TOKENS
}

fn latest_checkpoint_summary_for_work_item(
    work_item_id: &WorkItemId,
    domain: &ProjectionState,
) -> Option<String> {
    for event in domain.events.iter().rev().take(INSPECTOR_EVENT_SCAN_LIMIT) {
        if event.work_item_id.as_ref() != Some(work_item_id) {
            continue;
        }
        if let OrchestrationEventPayload::SessionCheckpoint(payload) = &event.payload {
            return Some(compact_focus_card_text(payload.summary.as_str()));
        }
    }
    None
}

fn latest_blocked_reason_for_work_item(
    work_item_id: &WorkItemId,
    domain: &ProjectionState,
) -> Option<String> {
    for event in domain.events.iter().rev().take(INSPECTOR_EVENT_SCAN_LIMIT) {
        if event.work_item_id.as_ref() != Some(work_item_id) {
            continue;
        }
        if let OrchestrationEventPayload::SessionBlocked(payload) = &event.payload {
            return Some(compact_focus_card_text(payload.reason.as_str()));
        }
    }
    None
}
