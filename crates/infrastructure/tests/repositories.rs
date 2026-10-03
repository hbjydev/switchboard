#![expect(
    clippy::panic_in_result_fn,
    reason = "tests assert behavior while propagating setup errors"
)]

use std::sync::Arc;
use switchboard_agent::{AgentDefinition, ModelRef};
use switchboard_application::repository::{
    AgentRepository, ConversationRepository, MessageRepository, PeerRepository, RepositoryError,
    StoredMessage,
};
use switchboard_infrastructure::memory::{
    InMemoryAgentRepository, InMemoryConversationRepository, InMemoryMessageRepository,
    InMemoryPeerRepository,
};
use switchboard_kernel::{
    conversation::{Conversation, ParticipantRole},
    event::{DomainEvent, Envelope, MessageCreatedEvent, PeerJoinedConversationEvent},
    message::{Message, MessageContent},
    peer::{Peer, PeerAlias, PeerKind},
};
use switchboard_uuids::{ConversationId, EventId, MessageId, PeerId};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn alias(scope: &str) -> PeerAlias {
    PeerAlias {
        provider: "test".into(),
        scope: scope.into(),
        external_id: "alice".into(),
    }
}

fn entry(
    conversation: ConversationId,
    author: PeerId,
    text: &str,
) -> Result<StoredMessage, switchboard_kernel::DomainError> {
    let message = Message::new(conversation, author, MessageContent::Text(text.into()), [])?;
    let event = Envelope::root(
        author,
        DomainEvent::MessageCreated(MessageCreatedEvent {
            message_id: message.id(),
            conversation_id: conversation,
            author,
        }),
    );
    Ok(StoredMessage { message, event })
}

#[tokio::test]
async fn peers_preserve_scoped_aliases_and_remove_stale_entries() -> TestResult {
    let repository = InMemoryPeerRepository::default();
    assert!(repository.get(PeerId::new()).await?.is_none());
    assert!(repository.resolve_alias(&alias("a")).await?.is_none());
    let original = Peer::new("same name", PeerKind::Human)?;
    let mut first = original.clone();
    first.add_alias(alias("a"));
    let mut second = Peer::new("same name", PeerKind::Human)?;
    second.add_alias(alias("b"));
    let other_provider = PeerAlias {
        provider: "other".into(),
        ..alias("a")
    };
    second.add_alias(other_provider.clone());
    repository.save(first.clone()).await?;
    repository.save(first.clone()).await?;
    repository.save(second.clone()).await?;
    assert_eq!(
        repository.resolve_alias(&alias("a")).await?.map(|p| p.id()),
        Some(first.id())
    );
    assert_eq!(
        repository.resolve_alias(&alias("b")).await?.map(|p| p.id()),
        Some(second.id())
    );
    assert_eq!(
        repository
            .resolve_alias(&other_provider)
            .await?
            .map(|p| p.id()),
        Some(second.id())
    );
    repository.save(original).await?;
    assert!(repository.resolve_alias(&alias("a")).await?.is_none());
    assert_eq!(
        repository.get(first.id()).await?.map(|p| p.aliases().len()),
        Some(0)
    );
    Ok(())
}

#[tokio::test]
async fn alias_conflicts_leave_existing_state_unchanged() -> TestResult {
    let repository = InMemoryPeerRepository::default();
    let mut owner = Peer::new("owner", PeerKind::Human)?;
    owner.add_alias(alias("a"));
    let mut contender = Peer::new("contender", PeerKind::Human)?;
    contender.add_alias(alias("b"));
    repository.save(owner.clone()).await?;
    repository.save(contender.clone()).await?;
    let mut conflicting = contender.clone();
    conflicting.rename("changed")?;
    conflicting.add_alias(alias("c"));
    conflicting.add_alias(alias("a"));
    assert_eq!(
        repository.save(conflicting).await,
        Err(RepositoryError::Conflict)
    );
    assert_eq!(
        repository
            .get(contender.id())
            .await?
            .map(|p| p.display_name().to_owned()),
        Some("contender".into())
    );
    assert!(repository.resolve_alias(&alias("c")).await?.is_none());
    assert_eq!(
        repository.resolve_alias(&alias("a")).await?.map(|p| p.id()),
        Some(owner.id())
    );
    assert_eq!(
        repository.resolve_alias(&alias("b")).await?.map(|p| p.id()),
        Some(contender.id())
    );
    Ok(())
}

#[tokio::test]
async fn conversations_and_definitions_can_be_saved_and_replaced() -> TestResult {
    let conversations = InMemoryConversationRepository::default();
    let agents = InMemoryAgentRepository::default();
    let peer = Peer::new("agent", PeerKind::Agent)?;
    let mut conversation = Conversation::new([peer.id()]);
    assert!(conversations.get(conversation.id()).await?.is_none());
    conversations.save(conversation.clone()).await?;
    let observer = PeerId::new();
    conversation.add_participant(observer, ParticipantRole::Observer);
    conversations.save(conversation.clone()).await?;
    assert_eq!(
        conversations
            .get(conversation.id())
            .await?
            .map(|c| c.participants().len()),
        Some(2)
    );
    assert!(agents.get(peer.id()).await?.is_none());
    for instructions in ["first", "updated"] {
        agents
            .save(AgentDefinition::new(
                &peer,
                instructions,
                ModelRef {
                    provider: "fake".into(),
                    model: "test".into(),
                },
            )?)
            .await?;
    }
    assert_eq!(
        agents
            .get(peer.id())
            .await?
            .map(|a| a.instructions().to_owned()),
        Some("updated".into())
    );
    Ok(())
}

#[tokio::test]
async fn message_history_is_an_inclusive_append_order_prefix() -> TestResult {
    let repository = InMemoryMessageRepository::default();
    let conversation = ConversationId::new();
    let other = ConversationId::new();
    let author = PeerId::new();
    let first = entry(conversation, author, "first")?;
    let second = entry(conversation, author, "second")?;
    let third = entry(conversation, author, "later arrival")?;
    assert!(repository.history(conversation).await?.is_empty());
    assert!(repository.get(first.message.id()).await?.is_none());
    assert!(
        repository
            .history_through(conversation, first.message.id())
            .await?
            .is_none()
    );
    // Reverse creation order deliberately: append order wins over message time.
    repository.append(second.clone()).await?;
    repository
        .append(entry(other, author, "another conversation")?)
        .await?;
    repository.append(first.clone()).await?;
    let boundary = repository
        .history_through(conversation, first.message.id())
        .await?;
    repository.append(third.clone()).await?;
    let ids = |entries: Vec<StoredMessage>| {
        entries
            .into_iter()
            .map(|e| e.message.id())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        boundary.map(ids),
        Some(vec![second.message.id(), first.message.id()])
    );
    assert_eq!(
        repository
            .history_through(conversation, first.message.id())
            .await?
            .map(ids),
        Some(vec![second.message.id(), first.message.id()])
    );
    assert_eq!(
        ids(repository.history(conversation).await?),
        vec![second.message.id(), first.message.id(), third.message.id()]
    );
    assert!(
        repository
            .history_through(other, first.message.id())
            .await?
            .is_none()
    );
    assert!(
        repository
            .history_through(conversation, MessageId::new())
            .await?
            .is_none()
    );
    let stored = repository.get(first.message.id()).await?;
    assert_eq!(stored.map(|e| e.event.id), Some(first.event.id));
    Ok(())
}

#[tokio::test]
async fn invalid_messages_and_duplicate_ids_never_partially_append() -> TestResult {
    let repository = InMemoryMessageRepository::default();
    let conversation = ConversationId::new();
    let author = PeerId::new();
    let valid = entry(conversation, author, "valid")?;
    let mut invalid = Vec::new();
    for field in 0..4 {
        let mut value = valid.clone();
        if let DomainEvent::MessageCreated(payload) = &mut value.event.payload {
            match field {
                0 => payload.message_id = MessageId::new(),
                1 => payload.conversation_id = ConversationId::new(),
                2 => payload.author = PeerId::new(),
                _ => value.event.sender = PeerId::new(),
            }
        }
        invalid.push(value);
    }
    let mut wrong_kind = valid.clone();
    wrong_kind.event.payload = DomainEvent::PeerJoinedConversation(PeerJoinedConversationEvent {
        peer_id: author,
        conversation_id: conversation,
    });
    invalid.push(wrong_kind);
    let mut wrong_root = valid.clone();
    wrong_root.event.correlation = EventId::new();
    invalid.push(wrong_root);
    for value in invalid {
        assert_eq!(
            repository.append(value).await,
            Err(RepositoryError::InvalidData)
        );
        assert!(repository.history(conversation).await?.is_empty());
        assert!(repository.get(valid.message.id()).await?.is_none());
    }
    repository.append(valid.clone()).await?;
    assert_eq!(
        repository.append(valid.clone()).await,
        Err(RepositoryError::Conflict)
    );
    let mut duplicate_message = valid.clone();
    duplicate_message.event.id = EventId::new();
    duplicate_message.event.correlation = duplicate_message.event.id;
    assert_eq!(
        repository.append(duplicate_message).await,
        Err(RepositoryError::Conflict)
    );
    let mut duplicate_event = entry(conversation, author, "duplicate event")?;
    let rejected_id = duplicate_event.message.id();
    duplicate_event.event.id = valid.event.id;
    duplicate_event.event.correlation = valid.event.id;
    assert_eq!(
        repository.append(duplicate_event).await,
        Err(RepositoryError::Conflict)
    );
    assert!(repository.get(rejected_id).await?.is_none());
    assert_eq!(repository.history(conversation).await?.len(), 1);
    let mut reply = entry(conversation, author, "reply")?;
    reply.event = Envelope::reply(author, reply.event.payload, &valid.event);
    repository.append(reply.clone()).await?;
    let stored = repository.get(reply.message.id()).await?;
    assert_eq!(
        stored.as_ref().and_then(|e| e.event.causation),
        Some(valid.event.id)
    );
    assert_eq!(
        stored.map(|e| e.event.correlation),
        Some(valid.event.correlation)
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_alias_claims_and_duplicate_appends_have_one_winner() -> TestResult {
    let peers = Arc::new(InMemoryPeerRepository::default());
    let mut first = Peer::new("one", PeerKind::Human)?;
    let mut second = Peer::new("two", PeerKind::Human)?;
    first.add_alias(alias("shared"));
    second.add_alias(alias("shared"));
    let tasks = [first, second].map(|peer| {
        let peers = Arc::clone(&peers);
        tokio::spawn(async move { peers.save(peer).await })
    });
    let mut successes = 0;
    for task in tasks {
        match task.await? {
            Ok(()) => successes += 1,
            Err(error) => assert_eq!(error, RepositoryError::Conflict),
        }
    }
    assert_eq!(successes, 1);
    let messages = Arc::new(InMemoryMessageRepository::default());
    let value = entry(ConversationId::new(), PeerId::new(), "once")?;
    let conversation = value.message.conversation_id();
    let tasks = [value.clone(), value].map(|value| {
        let messages = Arc::clone(&messages);
        tokio::spawn(async move { messages.append(value).await })
    });
    let mut successes = 0;
    for task in tasks {
        match task.await? {
            Ok(()) => successes += 1,
            Err(error) => assert_eq!(error, RepositoryError::Conflict),
        }
    }
    assert_eq!(successes, 1);
    assert_eq!(messages.history(conversation).await?.len(), 1);
    Ok(())
}
