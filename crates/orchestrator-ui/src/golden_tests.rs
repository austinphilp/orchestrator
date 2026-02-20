use super::*;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use orchestrator_core::{
    rebuild_projection, NewEventEnvelope, OrchestrationEventPayload, StoredEventEnvelope,
};
use serde::Deserialize;
use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

const FIXTURE_DIR: &str = "tests/golden/ui_state/fixtures";
const SNAPSHOT_DIR: &str = "tests/golden/ui_state/snapshots";
const UPDATE_GOLDENS_ENV: &str = "ORCHESTRATOR_UPDATE_GOLDENS";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GoldenUiFixture {
    #[serde(default = "default_status")]
    status: String,
    #[serde(default)]
    selected_inbox_item_id: Option<InboxItemId>,
    #[serde(default)]
    center_view_stack: Vec<FixtureCenterView>,
    #[serde(default)]
    mode: FixtureUiMode,
    #[serde(default)]
    key_sequence: Vec<String>,
    events: Vec<FixtureEvent>,
}

fn default_status() -> String {
    "ready".to_owned()
}

#[derive(Debug, Clone, Copy, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum FixtureUiMode {
    #[default]
    Normal,
    Insert,
    Terminal,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum FixtureCenterView {
    FocusCard {
        inbox_item_id: InboxItemId,
    },
    Terminal {
        session_id: WorkerSessionId,
    },
    Inspector {
        work_item_id: WorkItemId,
        inspector: FixtureInspectorKind,
    },
}

impl FixtureCenterView {
    fn into_center_view(self) -> CenterView {
        match self {
            Self::FocusCard { inbox_item_id } => CenterView::FocusCardView { inbox_item_id },
            Self::Terminal { session_id } => CenterView::TerminalView { session_id },
            Self::Inspector {
                work_item_id,
                inspector,
            } => CenterView::InspectorView {
                work_item_id,
                inspector: inspector.into_inspector_kind(),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FixtureInspectorKind {
    Diff,
    Test,
    PullRequest,
    Chat,
}

impl FixtureInspectorKind {
    fn into_inspector_kind(self) -> ArtifactInspectorKind {
        match self {
            Self::Diff => ArtifactInspectorKind::Diff,
            Self::Test => ArtifactInspectorKind::Test,
            Self::PullRequest => ArtifactInspectorKind::PullRequest,
            Self::Chat => ArtifactInspectorKind::Chat,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixtureEvent {
    sequence: u64,
    event_id: String,
    occurred_at: String,
    #[serde(default)]
    work_item_id: Option<WorkItemId>,
    #[serde(default)]
    session_id: Option<WorkerSessionId>,
    payload: OrchestrationEventPayload,
    #[serde(default = "default_schema_version")]
    schema_version: u32,
}

fn default_schema_version() -> u32 {
    1
}

impl FixtureEvent {
    fn into_stored(self) -> StoredEventEnvelope {
        StoredEventEnvelope::from((
            self.sequence,
            NewEventEnvelope {
                event_id: self.event_id,
                occurred_at: self.occurred_at,
                work_item_id: self.work_item_id,
                session_id: self.session_id,
                payload: self.payload,
                schema_version: self.schema_version,
            },
        ))
    }
}

#[test]
fn inbox_view_golden_snapshot_from_recorded_events() {
    assert_fixture_matches_snapshot("inbox_view.json");
}

#[test]
fn focus_card_golden_snapshot_from_recorded_events() {
    assert_fixture_matches_snapshot("focus_card_view.json");
}

#[test]
fn terminal_overlay_golden_snapshot_from_recorded_events() {
    assert_fixture_matches_snapshot("terminal_overlay_view.json");
}

fn assert_fixture_matches_snapshot(fixture_name: &str) {
    let fixture_path = fixture_path(fixture_name);
    let snapshot_name = fixture_name
        .strip_suffix(".json")
        .unwrap_or(fixture_name)
        .to_owned();
    let fixture = load_fixture(fixture_path.as_path());
    let actual = render_fixture_snapshot(fixture_name, fixture);

    let snapshot_path = snapshot_path(snapshot_name.as_str());
    assert_snapshot(
        snapshot_name.as_str(),
        snapshot_path.as_path(),
        actual.as_str(),
    );
}

fn load_fixture(path: &Path) -> GoldenUiFixture {
    let raw = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("failed to read fixture {}: {error}", path.display()));
    serde_json::from_str(raw.as_str())
        .unwrap_or_else(|error| panic!("failed to parse fixture {}: {error}", path.display()))
}

fn render_fixture_snapshot(fixture_name: &str, fixture: GoldenUiFixture) -> String {
    let events = validate_recorded_events(fixture_name, fixture.events);

    let projection = rebuild_projection(events.as_slice());
    let mut shell_state = UiShellState::new(fixture.status, projection);

    if let Some(selected_inbox_item_id) = fixture.selected_inbox_item_id {
        select_inbox_item_by_id(&mut shell_state, &selected_inbox_item_id, fixture_name);
    }
    if !fixture.center_view_stack.is_empty() {
        set_center_view_stack(&mut shell_state, fixture.center_view_stack);
    }
    apply_fixture_mode(&mut shell_state, fixture.mode, fixture_name);

    for token in fixture.key_sequence {
        let key = parse_fixture_key(token.as_str());
        let should_quit = handle_key_press(&mut shell_state, key);
        assert!(
            !should_quit,
            "fixture {fixture_name} unexpectedly triggered quit while applying key {}",
            token
        );
    }

    let ui_state = shell_state.ui_state();
    format_snapshot_text(&ui_state, &shell_state)
}

fn validate_recorded_events(
    fixture_name: &str,
    events: Vec<FixtureEvent>,
) -> Vec<StoredEventEnvelope> {
    let mut previous_sequence = None;
    let mut seen_event_ids = HashSet::new();
    let mut validated = Vec::with_capacity(events.len());

    for event in events {
        assert!(
            !event.event_id.trim().is_empty(),
            "fixture {fixture_name} contains an event with an empty event_id"
        );
        assert!(
            !event.occurred_at.trim().is_empty(),
            "fixture {fixture_name} event {} has an empty occurred_at timestamp",
            event.event_id
        );
        assert!(
            event.schema_version > 0,
            "fixture {fixture_name} event {} must use schema_version >= 1",
            event.event_id
        );
        assert!(
            seen_event_ids.insert(event.event_id.clone()),
            "fixture {fixture_name} contains duplicate event_id {}",
            event.event_id
        );

        if let Some(previous) = previous_sequence {
            assert!(
                event.sequence > previous,
                "fixture {fixture_name} event {} has sequence {} after {}; recorded event logs must be strictly increasing",
                event.event_id,
                event.sequence,
                previous
            );
        }
        previous_sequence = Some(event.sequence);

        validated.push(event.into_stored());
    }

    validated
}

fn select_inbox_item_by_id(
    shell_state: &mut UiShellState,
    inbox_item_id: &InboxItemId,
    fixture_name: &str,
) {
    let rows = shell_state.ui_state().inbox_rows;
    let selected_index = rows
        .iter()
        .position(|row| row.inbox_item_id == *inbox_item_id)
        .unwrap_or_else(|| {
            panic!(
                "fixture {fixture_name} selected_inbox_item_id={} was not present in inbox rows",
                inbox_item_id.as_str()
            )
        });

    shell_state.set_selection(Some(selected_index), rows.as_slice());
}

fn set_center_view_stack(shell_state: &mut UiShellState, configured_stack: Vec<FixtureCenterView>) {
    let mut stack = ViewStack::default();
    let mut views = configured_stack.into_iter();

    if let Some(first) = views.next() {
        stack.replace_center(first.into_center_view());
        for view in views {
            let _ = stack.push_center(view.into_center_view());
        }
    }

    shell_state.view_stack = stack;
}

fn apply_fixture_mode(shell_state: &mut UiShellState, mode: FixtureUiMode, fixture_name: &str) {
    match mode {
        FixtureUiMode::Normal => shell_state.enter_normal_mode(),
        FixtureUiMode::Insert => {
            shell_state.enter_insert_mode();
            assert_eq!(
                shell_state.mode,
                UiMode::Insert,
                "fixture {fixture_name} requested insert mode but shell did not enter insert mode"
            );
        }
        FixtureUiMode::Terminal => {
            shell_state.enter_terminal_mode();
            assert_eq!(
                shell_state.mode,
                UiMode::Terminal,
                "fixture {fixture_name} requested terminal mode but active center view is not terminal"
            );
        }
    }
}

fn parse_fixture_key(token: &str) -> KeyEvent {
    if let Some(chord) = token.strip_prefix("ctrl+") {
        let mut chars = chord.chars();
        let ch = chars
            .next()
            .unwrap_or_else(|| panic!("invalid ctrl key token: {token}"));
        assert!(
            chars.next().is_none(),
            "ctrl key token must map to a single character: {token}"
        );
        return KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL);
    }

    match token {
        "esc" => KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        "enter" => KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        "tab" => KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
        "backtab" => KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE),
        "up" => KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        "down" => KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        "left" => KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
        "right" => KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        _ => {
            let mut chars = token.chars();
            let ch = chars
                .next()
                .unwrap_or_else(|| panic!("empty key token is invalid"));
            assert!(
                chars.next().is_none(),
                "unsupported key token '{token}'; use single chars or named tokens"
            );
            KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE)
        }
    }
}

fn format_snapshot_text(ui_state: &UiState, shell_state: &UiShellState) -> String {
    let selected_index = ui_state
        .selected_inbox_index
        .map(|index| index.to_string())
        .unwrap_or_else(|| "none".to_owned());
    let selected_item = ui_state
        .selected_inbox_item_id
        .as_ref()
        .map(|id| id.as_str())
        .unwrap_or("none");
    let overlay = shell_state
        .which_key_overlay
        .as_ref()
        .map(render_which_key_overlay_text)
        .unwrap_or_else(|| "(none)".to_owned());

    let mut text = String::new();
    let _ = writeln!(text, "status: {}", ui_state.status);
    let _ = writeln!(text, "mode: {}", shell_state.mode.label());
    let _ = writeln!(text, "selected_inbox_index: {selected_index}");
    let _ = writeln!(text, "selected_inbox_item_id: {selected_item}");
    let _ = writeln!(text, "center_stack: {}", ui_state.center_stack_label());
    let _ = writeln!(text, "center_title: {}", ui_state.center_pane.title);

    text.push_str("\n[inbox]\n");
    text.push_str(render_inbox_panel(ui_state).as_str());
    text.push('\n');

    text.push_str("\n[center]\n");
    text.push_str(render_center_panel(ui_state).as_str());
    text.push('\n');

    text.push_str("\n[which-key]\n");
    text.push_str(overlay.as_str());
    text.push('\n');

    text
}

fn assert_snapshot(name: &str, path: &Path, actual: &str) {
    if should_update_goldens() {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap_or_else(|error| {
                panic!(
                    "failed to create snapshot directory {}: {error}",
                    parent.display()
                )
            });
        }
        fs::write(path, actual)
            .unwrap_or_else(|error| panic!("failed to write snapshot {}: {error}", path.display()));
        return;
    }

    let expected = fs::read_to_string(path).unwrap_or_else(|error| {
        panic!(
            "missing or unreadable snapshot for {name}: {} ({error}). Run with {UPDATE_GOLDENS_ENV}=1 to generate it.",
            path.display()
        )
    });

    if expected != actual {
        panic!(
            "snapshot mismatch for {name} ({}). Run with {UPDATE_GOLDENS_ENV}=1 to accept changes.\n\n{}",
            path.display(),
            first_diff_message(expected.as_str(), actual)
        );
    }
}

fn first_diff_message(expected: &str, actual: &str) -> String {
    let expected_lines = expected.lines().collect::<Vec<_>>();
    let actual_lines = actual.lines().collect::<Vec<_>>();
    let max_len = expected_lines.len().max(actual_lines.len());

    for index in 0..max_len {
        let expected_line = expected_lines.get(index).copied();
        let actual_line = actual_lines.get(index).copied();
        if expected_line != actual_line {
            return format!(
                "first difference at line {}\nexpected: {}\nactual: {}",
                index + 1,
                expected_line.unwrap_or("<missing>"),
                actual_line.unwrap_or("<missing>")
            );
        }
    }

    "snapshot content differs but no line diff was found".to_owned()
}

fn should_update_goldens() -> bool {
    std::env::var(UPDATE_GOLDENS_ENV)
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn fixture_path(file_name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(FIXTURE_DIR)
        .join(file_name)
}

fn snapshot_path(snapshot_name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(SNAPSHOT_DIR)
        .join(format!("{snapshot_name}.snap"))
}

#[test]
#[should_panic(expected = "strictly increasing")]
fn fixtures_reject_non_monotonic_sequences() {
    let mut fixture = load_fixture(fixture_path("inbox_view.json").as_path());
    fixture.events.swap(0, 1);
    let _ = render_fixture_snapshot("inbox_view.json", fixture);
}

#[test]
#[should_panic(expected = "duplicate event_id")]
fn fixtures_reject_duplicate_event_ids() {
    let mut fixture = load_fixture(fixture_path("inbox_view.json").as_path());
    fixture.events[1].event_id = fixture.events[0].event_id.clone();
    let _ = render_fixture_snapshot("inbox_view.json", fixture);
}

#[test]
fn fixture_schema_rejects_unknown_top_level_fields() {
    let raw = r#"
    {
      "status": "ready",
      "events": [],
      "unexpected_field": true
    }
    "#;
    let error =
        serde_json::from_str::<GoldenUiFixture>(raw).expect_err("unknown fields should fail");
    assert!(error.to_string().contains("unknown field"));
}
