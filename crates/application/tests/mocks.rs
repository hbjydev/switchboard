#![cfg(feature = "mocks")]
#![expect(
    clippy::panic_in_result_fn,
    reason = "tests assert results while propagating setup failures"
)]

use std::sync::Arc;
use switchboard_application::repository::{
    AgentRepository, ConversationRepository, MessageRepository, MockAgentRepository,
    MockConversationRepository, MockMessageRepository, MockPeerRepository, PeerRepository,
    RepositoryError,
};
use switchboard_kernel::{
    DomainError,
    peer::{Peer, PeerAlias, PeerKind},
};
use switchboard_uuids::{ConversationId, MessageId, PeerId};

#[tokio::test]
async fn peer_mock_matches_scoped_aliases_through_a_trait_object() -> Result<(), DomainError> {
    let peer = Peer::new("Alice", PeerKind::Human)?;
    let id = peer.id();
    let alias = PeerAlias {
        provider: "test".into(),
        scope: "account-a".into(),
        external_id: "alice".into(),
    };
    let mut mock = MockPeerRepository::new();
    mock.expect_resolve_alias()
        .withf(move |key| key == &alias)
        .times(1)
        .returning(move |_| Ok(Some(peer.clone())));
    let repository: Arc<dyn PeerRepository> = Arc::new(mock);
    let resolved = repository
        .resolve_alias(&PeerAlias {
            provider: "test".into(),
            scope: "account-a".into(),
            external_id: "alice".into(),
        })
        .await;
    assert!(matches!(resolved, Ok(Some(peer)) if peer.id() == id));
    Ok(())
}

#[tokio::test]
async fn other_repository_mocks_support_async_results_and_boundaries() {
    let conversation = ConversationId::new();
    let through = MessageId::new();
    let agent = PeerId::new();
    let mut conversations = MockConversationRepository::new();
    conversations
        .expect_get()
        .with(mockall::predicate::eq(conversation))
        .times(1)
        .returning(|_| Ok(None));
    let conversations: Arc<dyn ConversationRepository> = Arc::new(conversations);
    assert!(matches!(conversations.get(conversation).await, Ok(None)));

    let mut messages = MockMessageRepository::new();
    messages
        .expect_history_through()
        .with(
            mockall::predicate::eq(conversation),
            mockall::predicate::eq(through),
        )
        .times(1)
        .returning(|_, _| Ok(Some(Vec::new())));
    let messages: Arc<dyn MessageRepository> = Arc::new(messages);
    assert!(
        matches!(messages.history_through(conversation, through).await, Ok(Some(entries)) if entries.is_empty())
    );

    let mut agents = MockAgentRepository::new();
    agents
        .expect_get()
        .with(mockall::predicate::eq(agent))
        .times(1)
        .returning(|_| Err(RepositoryError::Unavailable));
    let agents: Arc<dyn AgentRepository> = Arc::new(agents);
    assert!(matches!(
        agents.get(agent).await,
        Err(RepositoryError::Unavailable)
    ));
}
