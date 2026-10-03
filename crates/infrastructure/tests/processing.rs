#![expect(
    clippy::panic_in_result_fn,
    reason = "tests assert behavior while propagating setup errors"
)]
use std::sync::Arc;
use switchboard_agent::{
    AgentDefinition, ModelRef,
    model::{GenerationResponse, ModelError},
    runtime::{AgentRuntime, RuntimeError},
};
use switchboard_application::{
    messaging::{SendMessage, SendMessageError, SendMessageRequest},
    processing::{ActivationProcessor, ProcessingError},
    repository::{
        AgentRepository, ConversationRepository, MessageRepository, PeerRepository, StoredMessage,
    },
};
use switchboard_infrastructure::{
    fake_model::FakeModel,
    memory::{
        InMemoryAgentRepository, InMemoryConversationRepository, InMemoryMessageRepository,
        InMemoryPeerRepository,
    },
};
use switchboard_kernel::{
    DomainError,
    conversation::Conversation,
    message::MessageContent,
    peer::{Peer, PeerKind},
};
use switchboard_uuids::PeerId;
type TestResult = Result<(), Box<dyn std::error::Error>>;
struct Fixture {
    processor: ActivationProcessor,
    sender: SendMessage,
    model: Arc<FakeModel>,
    messages: Arc<InMemoryMessageRepository>,
    conversation: Conversation,
    human: PeerId,
    agents: Vec<PeerId>,
}
impl Fixture {
    async fn new(count: usize) -> Result<Self, Box<dyn std::error::Error>> {
        let peers = Arc::new(InMemoryPeerRepository::default());
        let conversations = Arc::new(InMemoryConversationRepository::default());
        let messages = Arc::new(InMemoryMessageRepository::default());
        let agents = Arc::new(InMemoryAgentRepository::default());
        let model = Arc::new(FakeModel::default());
        let human = Peer::new("same name", PeerKind::Human)?;
        peers.save(human.clone()).await?;
        let mut ids = Vec::new();
        for _ in 0..count {
            let peer = Peer::new("same name", PeerKind::Agent)?;
            peers.save(peer.clone()).await?;
            agents
                .save(AgentDefinition::new(
                    &peer,
                    "instructions",
                    ModelRef {
                        provider: "fake".into(),
                        model: "test".into(),
                    },
                )?)
                .await?;
            ids.push(peer.id());
        }
        let conversation =
            Conversation::new(std::iter::once(human.id()).chain(ids.iter().copied()));
        conversations.save(conversation.clone()).await?;
        let sender = SendMessage::new(peers.clone(), conversations.clone(), messages.clone());
        let processor = ActivationProcessor::new(
            peers,
            conversations,
            messages.clone(),
            agents,
            AgentRuntime::new(model.clone()),
        );
        Ok(Self {
            processor,
            sender,
            model,
            messages,
            conversation,
            human: human.id(),
            agents: ids,
        })
    }
    async fn send(
        &self,
        author: PeerId,
        addressed_peers: Vec<PeerId>,
        text: &str,
    ) -> Result<StoredMessage, SendMessageError> {
        self.sender
            .execute(SendMessageRequest {
                conversation_id: self.conversation.id(),
                author,
                content: MessageContent::Text(text.into()),
                addressed_peers,
                reply_to: None,
            })
            .await
    }
}

#[tokio::test]
async fn direct_reply_is_correctly_authored_and_completed_activation_is_deduplicated() -> TestResult
{
    let mut fixture = Fixture::new(1).await?;
    let root = fixture.send(fixture.human, vec![], "hello").await?;
    let replies = fixture.processor.process(&root.event).await?;
    let reply = replies.first().ok_or("no reply")?;
    assert_eq!(replies.len(), 1);
    assert_eq!(
        reply.message.author(),
        *fixture.agents.first().ok_or("no agent")?
    );
    assert_eq!(reply.event.causation, Some(root.event.id));
    assert_eq!(reply.event.correlation, root.event.id);
    assert!(fixture.processor.process(&root.event).await?.is_empty());
    assert!(fixture.processor.process(&reply.event).await?.is_empty());
    assert_eq!(fixture.model.requests()?.len(), 1);
    assert_eq!(
        fixture
            .messages
            .history(fixture.conversation.id())
            .await?
            .len(),
        2
    );
    Ok(())
}

#[tokio::test]
async fn explicit_selection_uses_only_history_through_the_trigger() -> TestResult {
    let mut fixture = Fixture::new(2).await?;
    let selected = *fixture.agents.last().ok_or("no agent")?;
    let trigger = fixture
        .send(fixture.human, vec![selected], "trigger")
        .await?;
    fixture.send(fixture.human, vec![], "later arrival").await?;
    let replies = fixture.processor.process(&trigger.event).await?;
    assert_eq!(
        replies
            .iter()
            .map(|r| r.message.author())
            .collect::<Vec<_>>(),
        vec![selected]
    );
    let requests = fixture.model.requests()?;
    let request = requests.first().ok_or("no request")?;
    assert_eq!(request.messages.len(), 1);
    assert_eq!(
        request.messages.first().map(|m| m.message_id),
        Some(trigger.message.id())
    );
    assert_eq!(request.participants.len(), 3);
    assert_eq!(
        request.messages.first().map(|m| m.speaker),
        Some(fixture.human)
    );
    Ok(())
}

#[tokio::test]
async fn ignored_group_and_agent_messages_do_not_call_a_model() -> TestResult {
    let mut fixture = Fixture::new(2).await?;
    let group = fixture.send(fixture.human, vec![], "group").await?;
    assert!(fixture.processor.process(&group.event).await?.is_empty());
    for agent in fixture.agents.clone() {
        let message = fixture
            .send(agent, fixture.agents.clone(), "agent to agent and self")
            .await?;
        assert!(fixture.processor.process(&message.event).await?.is_empty());
    }
    assert!(fixture.model.requests()?.is_empty());
    assert_eq!(
        fixture
            .messages
            .history(fixture.conversation.id())
            .await?
            .len(),
        3
    );
    Ok(())
}

#[tokio::test]
async fn failed_generation_and_blank_output_preserve_original_and_allow_retry() -> TestResult {
    let mut fixture = Fixture::new(1).await?;
    let root = fixture.send(fixture.human, vec![], "hello").await?;
    fixture.model.enqueue(Err(ModelError::Unavailable))?;
    fixture
        .model
        .enqueue(Ok(GenerationResponse { text: " \n".into() }))?;
    assert!(matches!(
        fixture.processor.process(&root.event).await,
        Err(ProcessingError::Runtime(RuntimeError::Model(
            ModelError::Unavailable
        )))
    ));
    assert_eq!(
        fixture
            .messages
            .history(fixture.conversation.id())
            .await?
            .len(),
        1
    );
    assert!(matches!(
        fixture.processor.process(&root.event).await,
        Err(ProcessingError::Send(SendMessageError::Domain(
            DomainError::BlankMessage
        )))
    ));
    assert_eq!(
        fixture
            .messages
            .history(fixture.conversation.id())
            .await?
            .len(),
        1
    );
    assert_eq!(fixture.processor.process(&root.event).await?.len(), 1);
    assert!(fixture.processor.process(&root.event).await?.is_empty());
    assert_eq!(fixture.model.requests()?.len(), 3);
    Ok(())
}

#[tokio::test]
async fn partial_success_retry_skips_completed_agent_and_keeps_original_history() -> TestResult {
    let mut fixture = Fixture::new(2).await?;
    let root = fixture
        .send(
            fixture.human,
            fixture.agents.iter().rev().copied().collect(),
            "hello",
        )
        .await?;
    fixture.model.enqueue(Ok(GenerationResponse {
        text: "first reply".into(),
    }))?;
    fixture.model.enqueue(Err(ModelError::Timeout))?;
    assert!(matches!(
        fixture.processor.process(&root.event).await,
        Err(ProcessingError::Runtime(RuntimeError::Model(
            ModelError::Timeout
        )))
    ));
    assert_eq!(
        fixture
            .messages
            .history(fixture.conversation.id())
            .await?
            .len(),
        2
    );
    let replies = fixture.processor.process(&root.event).await?;
    assert_eq!(
        replies
            .iter()
            .map(|r| r.message.author())
            .collect::<Vec<_>>(),
        fixture.agents.iter().skip(1).copied().collect::<Vec<_>>()
    );
    let requests = fixture.model.requests()?;
    assert_eq!(
        requests.iter().map(|r| r.acting_peer).collect::<Vec<_>>(),
        fixture
            .agents
            .iter()
            .copied()
            .chain(fixture.agents.iter().skip(1).copied())
            .collect::<Vec<_>>()
    );
    assert!(requests.iter().all(|r| r.messages.len() == 1));
    assert!(fixture.processor.process(&root.event).await?.is_empty());
    assert_eq!(
        fixture
            .messages
            .history(fixture.conversation.id())
            .await?
            .len(),
        3
    );
    Ok(())
}

#[tokio::test]
async fn forged_event_is_rejected_before_generation() -> TestResult {
    let mut fixture = Fixture::new(1).await?;
    let root = fixture.send(fixture.human, vec![], "hello").await?;
    let mut forged = root.event.clone();
    forged.sender = PeerId::new();
    assert!(matches!(
        fixture.processor.process(&forged).await,
        Err(ProcessingError::InvalidEvent)
    ));
    assert!(fixture.model.requests()?.is_empty());
    assert_eq!(
        fixture
            .messages
            .history(fixture.conversation.id())
            .await?
            .len(),
        1
    );
    Ok(())
}
