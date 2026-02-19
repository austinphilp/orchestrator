fn provider_to_str(provider: &TicketProvider) -> &'static str {
    match provider {
        TicketProvider::Linear => "linear",
        TicketProvider::Shortcut => "shortcut",
    }
}

fn str_to_provider(value: &str) -> Result<TicketProvider, rusqlite::Error> {
    match value {
        "linear" => Ok(TicketProvider::Linear),
        "shortcut" => Ok(TicketProvider::Shortcut),
        other => Err(to_from_sql_error(std::io::Error::other(format!(
            "unknown ticket provider: {other}"
        )))),
    }
}

fn validate_ticket_identity(ticket: &TicketRecord) -> Result<(), CoreError> {
    let expected =
        TicketId::from_provider_uuid(ticket.provider.clone(), &ticket.provider_ticket_id);
    if ticket.ticket_id == expected {
        return Ok(());
    }

    Err(CoreError::Persistence(format!(
        "ticket_id '{}' does not match canonical id '{}' for provider '{}' and provider_ticket_id '{}'",
        ticket.ticket_id.as_str(),
        expected.as_str(),
        provider_to_str(&ticket.provider),
        ticket.provider_ticket_id
    )))
}

fn backend_kind_to_json(kind: &BackendKind) -> Result<String, CoreError> {
    serde_json::to_string(kind).map_err(|err| CoreError::Persistence(err.to_string()))
}

fn str_to_backend_kind(value: &str) -> Result<BackendKind, rusqlite::Error> {
    serde_json::from_str(value).map_err(to_from_sql_error)
}

fn session_status_to_json(status: &WorkerSessionStatus) -> Result<String, CoreError> {
    serde_json::to_string(status).map_err(|err| CoreError::Persistence(err.to_string()))
}

fn str_to_session_status(value: &str) -> Result<WorkerSessionStatus, rusqlite::Error> {
    serde_json::from_str(value).map_err(to_from_sql_error)
}

fn to_from_sql_error<E>(err: E) -> rusqlite::Error
where
    E: std::error::Error + Send + Sync + 'static,
{
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(err))
}
