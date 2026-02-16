use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::CoreError;

/// Stable command identifiers shared across every orchestration invocation surface.
///
/// These IDs are public API vocabulary. They must be treated as stable contracts and
/// must not be renamed silently.
pub mod ids {
    pub const UI_FOCUS_NEXT_INBOX: &str = "ui.focus_next_inbox";
    pub const UI_OPEN_TERMINAL_FOR_SELECTED: &str = "ui.open_terminal_for_selected";
    pub const UI_OPEN_DIFF_INSPECTOR_FOR_SELECTED: &str = "ui.open_diff_inspector_for_selected";
    pub const UI_OPEN_TEST_INSPECTOR_FOR_SELECTED: &str = "ui.open_test_inspector_for_selected";
    pub const UI_OPEN_PR_INSPECTOR_FOR_SELECTED: &str = "ui.open_pr_inspector_for_selected";
    pub const UI_OPEN_CHAT_INSPECTOR_FOR_SELECTED: &str = "ui.open_chat_inspector_for_selected";
    pub const SUPERVISOR_QUERY: &str = "supervisor.query";
    pub const WORKFLOW_APPROVE_PR_READY: &str = "workflow.approve_pr_ready";
    pub const GITHUB_OPEN_REVIEW_TABS: &str = "github.open_review_tabs";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandArgSummary {
    None,
    SupervisorQuery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandMetadata {
    pub id: &'static str,
    pub description: &'static str,
    pub args: CommandArgSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SupervisorQueryArgs {
    Template {
        template: String,
        #[serde(default)]
        variables: BTreeMap<String, String>,
    },
    Freeform {
        query: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Command {
    UiFocusNextInbox,
    UiOpenTerminalForSelected,
    UiOpenDiffInspectorForSelected,
    UiOpenTestInspectorForSelected,
    UiOpenPrInspectorForSelected,
    UiOpenChatInspectorForSelected,
    SupervisorQuery(SupervisorQueryArgs),
    WorkflowApprovePrReady,
    GithubOpenReviewTabs,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UntypedCommandInvocation {
    pub command_id: String,
    #[serde(default)]
    pub args: Option<Value>,
}

#[derive(Clone, Debug)]
pub struct CommandDefinition {
    metadata: CommandMetadata,
    parse: fn(Option<&Value>) -> Result<Command, CoreError>,
    to_untyped_args: fn(&Command) -> Option<Value>,
}

impl CommandDefinition {
    fn new(
        metadata: CommandMetadata,
        parse: fn(Option<&Value>) -> Result<Command, CoreError>,
        to_untyped_args: fn(&Command) -> Option<Value>,
    ) -> Self {
        Self {
            metadata,
            parse,
            to_untyped_args,
        }
    }

    pub fn metadata(&self) -> &CommandMetadata {
        &self.metadata
    }
}

#[derive(Debug)]
pub struct CommandRegistry {
    definitions: BTreeMap<&'static str, CommandDefinition>,
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new().expect("canonical command registry should not contain duplicates")
    }
}

impl CommandRegistry {
    pub fn new() -> Result<Self, CoreError> {
        Self::from_definitions(canonical_definitions())
    }

    pub fn from_definitions(definitions: Vec<CommandDefinition>) -> Result<Self, CoreError> {
        let mut mapped = BTreeMap::new();
        for definition in definitions {
            let id = definition.metadata.id;
            if mapped.insert(id, definition).is_some() {
                return Err(CoreError::DuplicateCommandId {
                    command_id: id.to_owned(),
                });
            }
        }

        Ok(Self {
            definitions: mapped,
        })
    }

    pub fn lookup(&self, command_id: &str) -> Result<&CommandMetadata, CoreError> {
        self.definitions
            .get(command_id)
            .map(CommandDefinition::metadata)
            .ok_or_else(|| CoreError::UnknownCommand {
                command_id: command_id.to_owned(),
            })
    }

    pub fn list(&self) -> Vec<&CommandMetadata> {
        self.definitions
            .values()
            .map(CommandDefinition::metadata)
            .collect()
    }

    pub fn parse_invocation(
        &self,
        invocation: &UntypedCommandInvocation,
    ) -> Result<Command, CoreError> {
        let definition = self
            .definitions
            .get(invocation.command_id.as_str())
            .ok_or_else(|| CoreError::UnknownCommand {
                command_id: invocation.command_id.clone(),
            })?;

        (definition.parse)(invocation.args.as_ref())
    }

    pub fn to_untyped_invocation(
        &self,
        command: &Command,
    ) -> Result<UntypedCommandInvocation, CoreError> {
        let command_id = command.id();
        let definition =
            self.definitions
                .get(command_id)
                .ok_or_else(|| CoreError::UnknownCommand {
                    command_id: command_id.to_owned(),
                })?;

        Ok(UntypedCommandInvocation {
            command_id: command_id.to_owned(),
            args: (definition.to_untyped_args)(command),
        })
    }
}

impl Command {
    pub fn id(&self) -> &'static str {
        match self {
            Command::UiFocusNextInbox => ids::UI_FOCUS_NEXT_INBOX,
            Command::UiOpenTerminalForSelected => ids::UI_OPEN_TERMINAL_FOR_SELECTED,
            Command::UiOpenDiffInspectorForSelected => ids::UI_OPEN_DIFF_INSPECTOR_FOR_SELECTED,
            Command::UiOpenTestInspectorForSelected => ids::UI_OPEN_TEST_INSPECTOR_FOR_SELECTED,
            Command::UiOpenPrInspectorForSelected => ids::UI_OPEN_PR_INSPECTOR_FOR_SELECTED,
            Command::UiOpenChatInspectorForSelected => ids::UI_OPEN_CHAT_INSPECTOR_FOR_SELECTED,
            Command::SupervisorQuery(_) => ids::SUPERVISOR_QUERY,
            Command::WorkflowApprovePrReady => ids::WORKFLOW_APPROVE_PR_READY,
            Command::GithubOpenReviewTabs => ids::GITHUB_OPEN_REVIEW_TABS,
        }
    }
}

fn canonical_definitions() -> Vec<CommandDefinition> {
    vec![
        CommandDefinition::new(
            CommandMetadata {
                id: ids::UI_FOCUS_NEXT_INBOX,
                description: "Focus the next inbox item in the UI.",
                args: CommandArgSummary::None,
            },
            |args| parse_zero_arg(ids::UI_FOCUS_NEXT_INBOX, args, Command::UiFocusNextInbox),
            |_| None,
        ),
        CommandDefinition::new(
            CommandMetadata {
                id: ids::UI_OPEN_TERMINAL_FOR_SELECTED,
                description: "Open a terminal for the currently selected item.",
                args: CommandArgSummary::None,
            },
            |args| {
                parse_zero_arg(
                    ids::UI_OPEN_TERMINAL_FOR_SELECTED,
                    args,
                    Command::UiOpenTerminalForSelected,
                )
            },
            |_| None,
        ),
        CommandDefinition::new(
            CommandMetadata {
                id: ids::UI_OPEN_DIFF_INSPECTOR_FOR_SELECTED,
                description: "Open the diff inspector for the currently selected item.",
                args: CommandArgSummary::None,
            },
            |args| {
                parse_zero_arg(
                    ids::UI_OPEN_DIFF_INSPECTOR_FOR_SELECTED,
                    args,
                    Command::UiOpenDiffInspectorForSelected,
                )
            },
            |_| None,
        ),
        CommandDefinition::new(
            CommandMetadata {
                id: ids::UI_OPEN_TEST_INSPECTOR_FOR_SELECTED,
                description: "Open the test inspector for the currently selected item.",
                args: CommandArgSummary::None,
            },
            |args| {
                parse_zero_arg(
                    ids::UI_OPEN_TEST_INSPECTOR_FOR_SELECTED,
                    args,
                    Command::UiOpenTestInspectorForSelected,
                )
            },
            |_| None,
        ),
        CommandDefinition::new(
            CommandMetadata {
                id: ids::UI_OPEN_PR_INSPECTOR_FOR_SELECTED,
                description: "Open the pull request inspector for the currently selected item.",
                args: CommandArgSummary::None,
            },
            |args| {
                parse_zero_arg(
                    ids::UI_OPEN_PR_INSPECTOR_FOR_SELECTED,
                    args,
                    Command::UiOpenPrInspectorForSelected,
                )
            },
            |_| None,
        ),
        CommandDefinition::new(
            CommandMetadata {
                id: ids::UI_OPEN_CHAT_INSPECTOR_FOR_SELECTED,
                description: "Open the chat inspector for the currently selected item.",
                args: CommandArgSummary::None,
            },
            |args| {
                parse_zero_arg(
                    ids::UI_OPEN_CHAT_INSPECTOR_FOR_SELECTED,
                    args,
                    Command::UiOpenChatInspectorForSelected,
                )
            },
            |_| None,
        ),
        CommandDefinition::new(
            CommandMetadata {
                id: ids::SUPERVISOR_QUERY,
                description: "Send a query to the supervisor using template or freeform args.",
                args: CommandArgSummary::SupervisorQuery,
            },
            parse_supervisor_query,
            |command| match command {
                Command::SupervisorQuery(args) => {
                    Some(serde_json::to_value(args).expect("supervisor args should serialize"))
                }
                _ => None,
            },
        ),
        CommandDefinition::new(
            CommandMetadata {
                id: ids::WORKFLOW_APPROVE_PR_READY,
                description: "Approve the current workflow item as PR-ready.",
                args: CommandArgSummary::None,
            },
            |args| {
                parse_zero_arg(
                    ids::WORKFLOW_APPROVE_PR_READY,
                    args,
                    Command::WorkflowApprovePrReady,
                )
            },
            |_| None,
        ),
        CommandDefinition::new(
            CommandMetadata {
                id: ids::GITHUB_OPEN_REVIEW_TABS,
                description: "Open GitHub review tabs for the active pull request.",
                args: CommandArgSummary::None,
            },
            |args| {
                parse_zero_arg(
                    ids::GITHUB_OPEN_REVIEW_TABS,
                    args,
                    Command::GithubOpenReviewTabs,
                )
            },
            |_| None,
        ),
    ]
}

fn parse_zero_arg(
    command_id: &'static str,
    args: Option<&Value>,
    command: Command,
) -> Result<Command, CoreError> {
    ensure_no_args(command_id, args)?;
    Ok(command)
}

fn parse_supervisor_query(args: Option<&Value>) -> Result<Command, CoreError> {
    let args = args.ok_or_else(|| CoreError::InvalidCommandArgs {
        command_id: ids::SUPERVISOR_QUERY.to_owned(),
        reason: "missing args payload; expected template/freeform object".to_owned(),
    })?;

    let parsed: SupervisorQueryArgs =
        serde_json::from_value(args.clone()).map_err(|err| CoreError::CommandSchemaMismatch {
            command_id: ids::SUPERVISOR_QUERY.to_owned(),
            expected: "{kind: 'template', template: string, variables?: object<string,string>} or {kind: 'freeform', query: string}".to_owned(),
            details: err.to_string(),
        })?;

    Ok(Command::SupervisorQuery(parsed))
}

fn ensure_no_args(command_id: &'static str, args: Option<&Value>) -> Result<(), CoreError> {
    if matches!(args, None | Some(Value::Null)) {
        return Ok(());
    }

    Err(CoreError::InvalidCommandArgs {
        command_id: command_id.to_owned(),
        reason: "this command does not accept args".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn known_command_lookup_returns_metadata() {
        let registry = CommandRegistry::new().expect("registry");
        for command_id in [
            ids::UI_FOCUS_NEXT_INBOX,
            ids::UI_OPEN_TERMINAL_FOR_SELECTED,
            ids::UI_OPEN_DIFF_INSPECTOR_FOR_SELECTED,
            ids::UI_OPEN_TEST_INSPECTOR_FOR_SELECTED,
            ids::UI_OPEN_PR_INSPECTOR_FOR_SELECTED,
            ids::UI_OPEN_CHAT_INSPECTOR_FOR_SELECTED,
            ids::SUPERVISOR_QUERY,
            ids::WORKFLOW_APPROVE_PR_READY,
            ids::GITHUB_OPEN_REVIEW_TABS,
        ] {
            let metadata = registry.lookup(command_id).expect("metadata");
            assert_eq!(metadata.id, command_id);
            assert!(!metadata.description.is_empty());
        }
    }

    #[test]
    fn unknown_command_returns_error() {
        let registry = CommandRegistry::new().expect("registry");
        let err = registry
            .lookup("ui.missing")
            .expect_err("unknown should fail");

        assert!(matches!(err, CoreError::UnknownCommand { .. }));
    }

    #[test]
    fn zero_arg_command_rejects_unexpected_args() {
        let registry = CommandRegistry::new().expect("registry");
        let invocation = UntypedCommandInvocation {
            command_id: ids::UI_FOCUS_NEXT_INBOX.to_owned(),
            args: Some(json!({"unexpected": true})),
        };

        let err = registry
            .parse_invocation(&invocation)
            .expect_err("args should be rejected");
        assert!(matches!(err, CoreError::InvalidCommandArgs { .. }));
    }

    #[test]
    fn supervisor_query_accepts_template_args() {
        let registry = CommandRegistry::new().expect("registry");
        let invocation = UntypedCommandInvocation {
            command_id: ids::SUPERVISOR_QUERY.to_owned(),
            args: Some(json!({
                "kind": "template",
                "template": "summarize_ticket",
                "variables": {
                    "ticket_id": "AP-96"
                }
            })),
        };

        let parsed = registry
            .parse_invocation(&invocation)
            .expect("valid template");
        assert_eq!(
            parsed,
            Command::SupervisorQuery(SupervisorQueryArgs::Template {
                template: "summarize_ticket".to_owned(),
                variables: BTreeMap::from([("ticket_id".to_owned(), "AP-96".to_owned())]),
            })
        );
    }

    #[test]
    fn supervisor_query_accepts_freeform_args() {
        let registry = CommandRegistry::new().expect("registry");
        let invocation = UntypedCommandInvocation {
            command_id: ids::SUPERVISOR_QUERY.to_owned(),
            args: Some(json!({
                "kind": "freeform",
                "query": "What changed in this ticket?"
            })),
        };

        let parsed = registry
            .parse_invocation(&invocation)
            .expect("valid freeform");
        assert_eq!(
            parsed,
            Command::SupervisorQuery(SupervisorQueryArgs::Freeform {
                query: "What changed in this ticket?".to_owned(),
            })
        );
    }

    #[test]
    fn supervisor_query_rejects_invalid_shape() {
        let registry = CommandRegistry::new().expect("registry");
        let invocation = UntypedCommandInvocation {
            command_id: ids::SUPERVISOR_QUERY.to_owned(),
            args: Some(json!({
                "kind": "template"
            })),
        };

        let err = registry
            .parse_invocation(&invocation)
            .expect_err("invalid shape");
        assert!(matches!(err, CoreError::CommandSchemaMismatch { .. }));
    }

    #[test]
    fn duplicate_command_ids_fail_fast() {
        let duplicate = CommandDefinition::new(
            CommandMetadata {
                id: ids::UI_FOCUS_NEXT_INBOX,
                description: "duplicate",
                args: CommandArgSummary::None,
            },
            |_| Ok(Command::UiFocusNextInbox),
            |_| None,
        );

        let err = CommandRegistry::from_definitions(vec![duplicate.clone(), duplicate])
            .expect_err("duplicate must fail");
        assert!(matches!(err, CoreError::DuplicateCommandId { .. }));
    }

    #[test]
    fn typed_round_trip_is_stable() {
        let registry = CommandRegistry::new().expect("registry");
        let typed = Command::SupervisorQuery(SupervisorQueryArgs::Template {
            template: "triage".to_owned(),
            variables: BTreeMap::from([("project".to_owned(), "orchestrator".to_owned())]),
        });

        let untyped = registry
            .to_untyped_invocation(&typed)
            .expect("to untyped invocation");
        let parsed = registry.parse_invocation(&untyped).expect("parse untyped");

        assert_eq!(parsed, typed);
    }

    #[test]
    fn inspector_commands_are_registered_as_zero_arg_commands() {
        let registry = CommandRegistry::new().expect("registry");
        for (command_id, expected) in [
            (
                ids::UI_OPEN_DIFF_INSPECTOR_FOR_SELECTED,
                Command::UiOpenDiffInspectorForSelected,
            ),
            (
                ids::UI_OPEN_TEST_INSPECTOR_FOR_SELECTED,
                Command::UiOpenTestInspectorForSelected,
            ),
            (
                ids::UI_OPEN_PR_INSPECTOR_FOR_SELECTED,
                Command::UiOpenPrInspectorForSelected,
            ),
            (
                ids::UI_OPEN_CHAT_INSPECTOR_FOR_SELECTED,
                Command::UiOpenChatInspectorForSelected,
            ),
        ] {
            let parsed = registry
                .parse_invocation(&UntypedCommandInvocation {
                    command_id: command_id.to_owned(),
                    args: None,
                })
                .expect("new inspector command should parse");
            assert_eq!(parsed, expected);

            let untyped = registry
                .to_untyped_invocation(&expected)
                .expect("new inspector command should serialize");
            assert_eq!(untyped.command_id, command_id);
            assert!(untyped.args.is_none());
        }
    }
}
