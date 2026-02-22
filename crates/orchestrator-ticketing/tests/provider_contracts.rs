use orchestrator_core::TicketingProvider as CoreTicketingProvider;
use orchestrator_ticketing::{
    AddTicketCommentRequest, CoreError, CreateTicketRequest, GetTicketRequest,
    LinearTicketingProvider, ShortcutTicketingProvider, TicketId, TicketProvider,
    TicketingProvider, TicketingProviderKind, UpdateTicketDescriptionRequest,
    UpdateTicketStateRequest,
};

async fn assert_shared_ticketing_contract<P>(
    provider: &P,
    expected_provider: TicketProvider,
    valid_ticket_id: TicketId,
    invalid_ticket_id: TicketId,
) where
    P: TicketingProvider + CoreTicketingProvider,
{
    let expected_kind = TicketingProviderKind::from_provider(expected_provider.clone());
    assert_eq!(CoreTicketingProvider::provider(provider), expected_provider);
    assert_eq!(provider.kind(), expected_kind);
    assert_eq!(provider.provider_key(), expected_kind.as_key());

    let create_error = provider
        .create_ticket(CreateTicketRequest {
            title: "   ".to_owned(),
            description: None,
            labels: Vec::new(),
            state: None,
            project: None,
            priority: None,
            assign_to_api_key_user: false,
        })
        .await
        .expect_err("empty title should be rejected");
    assert!(matches!(create_error, CoreError::Configuration(_)));

    let description_error = provider
        .update_ticket_description(UpdateTicketDescriptionRequest {
            ticket_id: valid_ticket_id.clone(),
            description: "   ".to_owned(),
        })
        .await
        .expect_err("empty ticket descriptions should be rejected");
    assert!(matches!(description_error, CoreError::Configuration(_)));

    let empty_comment_error = provider
        .add_comment(AddTicketCommentRequest {
            ticket_id: valid_ticket_id,
            comment: "   ".to_owned(),
            attachments: Vec::new(),
        })
        .await
        .expect_err("empty comments should be rejected");
    assert!(matches!(empty_comment_error, CoreError::Configuration(_)));

    let get_error = provider
        .get_ticket(GetTicketRequest {
            ticket_id: invalid_ticket_id.clone(),
        })
        .await
        .expect_err("provider mismatch ticket id should be rejected");
    assert!(matches!(get_error, CoreError::Configuration(_)));

    let state_error = provider
        .update_ticket_state(UpdateTicketStateRequest {
            ticket_id: invalid_ticket_id.clone(),
            state: "In Progress".to_owned(),
        })
        .await
        .expect_err("provider mismatch ticket id should be rejected");
    assert!(matches!(state_error, CoreError::Configuration(_)));

    let comment_error = provider
        .add_comment(AddTicketCommentRequest {
            ticket_id: invalid_ticket_id,
            comment: "contract comment".to_owned(),
            attachments: Vec::new(),
        })
        .await
        .expect_err("provider mismatch ticket id should be rejected");
    assert!(matches!(comment_error, CoreError::Configuration(_)));
}

#[tokio::test]
async fn linear_provider_satisfies_shared_ticketing_contract() {
    let provider = LinearTicketingProvider::scaffold_default();
    assert_shared_ticketing_contract(
        &provider,
        TicketProvider::Linear,
        TicketId::from_provider_uuid(TicketProvider::Linear, "123"),
        TicketId::from_provider_uuid(TicketProvider::Shortcut, "123"),
    )
    .await;
}

#[tokio::test]
async fn shortcut_provider_satisfies_shared_ticketing_contract() {
    let provider = ShortcutTicketingProvider::scaffold_default();
    assert_shared_ticketing_contract(
        &provider,
        TicketProvider::Shortcut,
        TicketId::from_provider_uuid(TicketProvider::Shortcut, "123"),
        TicketId::from_provider_uuid(TicketProvider::Linear, "123"),
    )
    .await;
}
