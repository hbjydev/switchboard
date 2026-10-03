#![expect(
    clippy::panic_in_result_fn,
    reason = "tests assert behavior while propagating setup failures"
)]
use std::sync::Arc;
use switchboard_application::{
    messaging::{SendMessage, SendMessageError, SendMessageRequest},
    repository::{ConversationRepository, MessageRepository, PeerRepository},
};
use switchboard_infrastructure::memory::{
    InMemoryConversationRepository, InMemoryMessageRepository, InMemoryPeerRepository,
};
use switchboard_kernel::{
    DomainError,
    conversation::{Conversation, ParticipantRole},
    event::DomainEvent,
    message::MessageContent,
    peer::{Peer, PeerKind},
};
use switchboard_uuids::{ConversationId, MessageId, PeerId};
type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Fixture {
    service: SendMessage,
    messages: Arc<InMemoryMessageRepository>,
    conversation: Conversation,
    human: PeerId,
    agent: PeerId,
    observer: PeerId,
    outsider: PeerId,
}
impl Fixture {
    async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let peers = Arc::new(InMemoryPeerRepository::default());
        let conversations = Arc::new(InMemoryConversationRepository::default());
        let messages = Arc::new(InMemoryMessageRepository::default());
        let human = Peer::new("human", PeerKind::Human)?;
        let agent = Peer::new("agent", PeerKind::Agent)?;
        let observer = Peer::new("observer", PeerKind::Human)?;
        let outsider = Peer::new("outsider", PeerKind::Service)?;
        let mut conversation = Conversation::new([human.id(), agent.id()]);
        conversation.add_participant(observer.id(), ParticipantRole::Observer);
        for peer in [&human, &agent, &observer, &outsider] {
            peers.save(peer.clone()).await?;
        }
        conversations.save(conversation.clone()).await?;
        Ok(Self {
            service: SendMessage::new(peers, conversations, messages.clone()),
            messages,
            conversation,
            human: human.id(),
            agent: agent.id(),
            observer: observer.id(),
            outsider: outsider.id(),
        })
    }
    fn request(&self) -> SendMessageRequest {
        SendMessageRequest {
            conversation_id: self.conversation.id(),
            author: self.human,
            content: MessageContent::Text("  hello\n".into()),
            addressed_peers: vec![self.agent],
            reply_to: None,
        }
    }
}

#[tokio::test]
async fn sends_commit_actual_authors_and_reply_provenance() -> TestResult {
    let fixture = Fixture::new().await?;
    let root = fixture.service.execute(fixture.request()).await?;
    assert_eq!(root.message.author(), fixture.human);
    assert_eq!(
        root.message.content(),
        &MessageContent::Text("  hello\n".into())
    );
    assert_eq!(root.message.addressed_peers(), &[fixture.agent]);
    assert_eq!(root.event.sender, fixture.human);
    assert_eq!(root.event.correlation, root.event.id);
    assert_eq!(root.event.causation, None);
    let DomainEvent::MessageCreated(payload) = &root.event.payload else {
        return Err("wrong event type".into());
    };
    assert_eq!(payload.message_id, root.message.id());
    assert_eq!(payload.conversation_id, fixture.conversation.id());
    assert_eq!(payload.author, fixture.human);
    let mut reply = fixture.request();
    reply.author = fixture.agent;
    reply.addressed_peers = vec![fixture.human];
    reply.reply_to = Some(root.message.id());
    let reply = fixture.service.execute(reply).await?;
    assert_eq!(reply.message.author(), fixture.agent);
    assert_eq!(reply.event.causation, Some(root.event.id));
    assert_eq!(reply.event.correlation, root.event.id);
    let history = fixture.messages.history(fixture.conversation.id()).await?;
    assert_eq!(
        history.iter().map(|e| e.message.id()).collect::<Vec<_>>(),
        vec![root.message.id(), reply.message.id()]
    );
    assert_eq!(history.last().map(|e| e.event.id), Some(reply.event.id));
    Ok(())
}

#[tokio::test]
async fn invalid_sends_return_typed_errors_without_storing_anything() -> TestResult {
    let fixture = Fixture::new().await?;
    let missing_conversation = ConversationId::new();
    let missing_author = PeerId::new();
    let missing_trigger = MessageId::new();
    let mut cases = Vec::new();
    let mut request = fixture.request();
    request.conversation_id = missing_conversation;
    cases.push((
        request,
        SendMessageError::ConversationNotFound(missing_conversation),
    ));
    let mut request = fixture.request();
    request.author = missing_author;
    cases.push((request, SendMessageError::AuthorNotFound(missing_author)));
    let mut request = fixture.request();
    request.author = fixture.outsider;
    cases.push((request, SendMessageError::NotParticipant(fixture.outsider)));
    let mut request = fixture.request();
    request.author = fixture.observer;
    cases.push((request, SendMessageError::CannotSend(fixture.observer)));
    let mut request = fixture.request();
    request.content = MessageContent::Text(" \n\t".into());
    cases.push((request, SendMessageError::Domain(DomainError::BlankMessage)));
    let mut request = fixture.request();
    request.addressed_peers = vec![fixture.agent, fixture.outsider];
    cases.push((
        request,
        SendMessageError::InvalidRecipient(fixture.outsider),
    ));
    let mut request = fixture.request();
    request.reply_to = Some(missing_trigger);
    cases.push((request, SendMessageError::TriggerNotFound(missing_trigger)));
    for (request, expected) in cases {
        assert!(matches!(fixture.service.execute(request).await, Err(error) if error == expected));
        assert!(
            fixture
                .messages
                .history(fixture.conversation.id())
                .await?
                .is_empty()
        );
        assert!(
            fixture
                .messages
                .history(missing_conversation)
                .await?
                .is_empty()
        );
    }
    Ok(())
}

#[tokio::test]
async fn replies_cannot_reference_messages_in_another_conversation() -> TestResult {
    let fixture = Fixture::new().await?;
    let other = Fixture::new().await?;
    let mut trigger = other.service.execute(other.request()).await?;
    // Both conversations share one message store for this test.
    fixture.messages.append(trigger.clone()).await?;
    let mut request = fixture.request();
    request.reply_to = Some(trigger.message.id());
    assert!(
        matches!(fixture.service.execute(request).await, Err(SendMessageError::TriggerOutsideConversation(id)) if id == trigger.message.id())
    );
    assert!(
        fixture
            .messages
            .history(fixture.conversation.id())
            .await?
            .is_empty()
    );
    // Owned retrieval cannot mutate the original event.
    trigger.event.sender = PeerId::new();
    assert_eq!(
        fixture
            .messages
            .get(trigger.message.id())
            .await?
            .map(|e| e.event.sender),
        Some(other.human)
    );
    Ok(())
}
