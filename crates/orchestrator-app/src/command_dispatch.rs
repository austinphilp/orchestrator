use orchestrator_core::{
    command_ids, resolve_supervisor_query_scope, Command, CommandRegistry, CoreError,
    LlmChatRequest, LlmProvider, LlmResponseStream, SupervisorQueryArgs,
    SupervisorQueryContextArgs, UntypedCommandInvocation,
};
use orchestrator_supervisor::{
    build_freeform_messages, build_template_messages_with_variables, SupervisorQueryEngine,
};
use orchestrator_ui::SupervisorCommandContext;

use crate::{open_event_store, supervisor_model_from_env};

fn invalid_supervisor_query_usage(reason: impl Into<String>) -> CoreError {
    CoreError::InvalidCommandArgs {
        command_id: command_ids::SUPERVISOR_QUERY.to_owned(),
        reason: reason.into(),
    }
}

fn merge_supervisor_query_context(
    fallback_context: SupervisorCommandContext,
    invocation_context: Option<SupervisorQueryContextArgs>,
) -> SupervisorCommandContext {
    let Some(invocation_context) = invocation_context else {
        return fallback_context;
    };

    let mut merged = fallback_context;

    let invocation_scope = invocation_context.scope;
    let has_explicit_scope = invocation_scope.is_some();
    let invocation_session_id = invocation_context.selected_session_id;
    let has_invocation_session = invocation_session_id.is_some();
    let invocation_work_item_id = invocation_context.selected_work_item_id;

    if let Some(scope) = invocation_scope {
        if scope == "global" {
            merged.selected_work_item_id = None;
            merged.selected_session_id = None;
        }
        merged.scope = Some(scope);
    }

    if let Some(session_id) = invocation_session_id {
        if !has_explicit_scope {
            merged.scope = Some(format!("session:{session_id}"));
        }
        merged.selected_session_id = Some(session_id);
    }

    if let Some(work_item_id) = invocation_work_item_id {
        if !has_explicit_scope && !has_invocation_session {
            merged.scope = Some(format!("work_item:{work_item_id}"));
            merged.selected_session_id = None;
        }
        merged.selected_work_item_id = Some(work_item_id);
    }

    merged
}

fn args_context(args: &SupervisorQueryArgs) -> Option<SupervisorQueryContextArgs> {
    match args {
        SupervisorQueryArgs::Template { context, .. } => context.clone(),
        SupervisorQueryArgs::Freeform { context, .. } => context.clone(),
    }
}

async fn execute_supervisor_query<P>(
    supervisor: &P,
    event_store_path: &str,
    args: SupervisorQueryArgs,
    fallback_context: SupervisorCommandContext,
) -> Result<(String, LlmResponseStream), CoreError>
where
    P: LlmProvider + Send + Sync + ?Sized,
{
    let context = merge_supervisor_query_context(fallback_context, args_context(&args));
    let scope = resolve_supervisor_query_scope(&context)?;
    let store = open_event_store(event_store_path)?;
    let query_engine = SupervisorQueryEngine::default();
    let context_pack = query_engine.build_context_pack_with_filters(&store, scope, &context)?;
    let messages = match args {
        SupervisorQueryArgs::Template {
            template,
            variables,
            ..
        } => build_template_messages_with_variables(template.as_str(), &variables, &context_pack)?,
        SupervisorQueryArgs::Freeform { query, .. } => {
            build_freeform_messages(query.as_str(), &context_pack)?
        }
    };

    supervisor
        .stream_chat(LlmChatRequest {
            model: supervisor_model_from_env(),
            messages,
            temperature: Some(0.2),
            max_output_tokens: Some(700),
        })
        .await
}

pub(crate) async fn dispatch_supervisor_runtime_command<P>(
    supervisor: &P,
    event_store_path: &str,
    invocation: UntypedCommandInvocation,
    context: SupervisorCommandContext,
) -> Result<(String, LlmResponseStream), CoreError>
where
    P: LlmProvider + Send + Sync + ?Sized,
{
    let command = CommandRegistry::default().parse_invocation(&invocation)?;
    match command {
        Command::SupervisorQuery(args) => {
            execute_supervisor_query(supervisor, event_store_path, args, context).await
        }
        command => Err(invalid_supervisor_query_usage(format!(
            "unsupported command '{}' for supervisor runtime dispatch; expected '{}'",
            command.id(),
            command_ids::SUPERVISOR_QUERY
        ))),
    }
}
