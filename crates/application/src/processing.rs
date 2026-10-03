//! Sequential, process-local activation orchestration.
use crate::{
    messaging::{SendMessage, SendMessageError, SendMessageRequest},
    repository::{
        AgentRepository, ConversationRepository, MessageRepository, PeerRepository,
        RepositoryError, StoredMessage,
    },
};
use std::{collections::HashSet, sync::Arc};
use switchboard_agent::{
    activation::responding_agents,
    runtime::{AgentRuntime, RuntimeError},
};
use switchboard_kernel::{
    event::{DomainEvent, Envelope},
    message::MessageContent,
    peer::{Peer, PeerKind},
};
use switchboard_uuids::{ConversationId, EventId, MessageId, PeerId};

pub struct ActivationProcessor {
    peers: Arc<dyn PeerRepository>,
    conversations: Arc<dyn ConversationRepository>,
    messages: Arc<dyn MessageRepository>,
    agents: Arc<dyn AgentRepository>,
    runtime: AgentRuntime,
    sender: SendMessage,
    completed: HashSet<(EventId, PeerId)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProcessingError {
    #[error("trigger message {0} does not exist")]
    MessageNotFound(MessageId),
    #[error("event does not match the committed message envelope")]
    InvalidEvent,
    #[error("conversation {0} does not exist")]
    ConversationNotFound(ConversationId),
    #[error("peer {0} does not exist")]
    PeerNotFound(PeerId),
    #[error("agent definition {0} does not match repository lookup")]
    InvalidDefinition(PeerId),
    #[error(transparent)]
    Repository(#[from] RepositoryError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    Send(#[from] SendMessageError),
}

impl ActivationProcessor {
    #[must_use]
    pub fn new(
        peers: Arc<dyn PeerRepository>,
        conversations: Arc<dyn ConversationRepository>,
        messages: Arc<dyn MessageRepository>,
        agents: Arc<dyn AgentRepository>,
        runtime: AgentRuntime,
    ) -> Self {
        let sender = SendMessage::new(peers.clone(), conversations.clone(), messages.clone());
        Self {
            peers,
            conversations,
            messages,
            agents,
            runtime,
            sender,
            completed: HashSet::new(),
        }
    }

    async fn peer(&self, id: PeerId) -> Result<Peer, ProcessingError> {
        self.peers
            .get(id)
            .await?
            .ok_or(ProcessingError::PeerNotFound(id))
    }

    /// Processes one committed event, returning only newly stored replies.
    ///
    /// A mutable receiver serializes calls on this instance. Earlier successful
    /// activations remain completed if a later agent fails. Retry the same event
    /// explicitly to resume; there are no background retries. State is not
    /// durable or shared with other processor instances.
    pub async fn process(
        &mut self,
        event: &Envelope,
    ) -> Result<Vec<StoredMessage>, ProcessingError> {
        let DomainEvent::MessageCreated(payload) = &event.payload else {
            return Ok(Vec::new());
        };
        let trigger = self
            .messages
            .get(payload.message_id)
            .await?
            .ok_or(ProcessingError::MessageNotFound(payload.message_id))?;
        if trigger.event != *event {
            return Err(ProcessingError::InvalidEvent);
        }
        let author = self.peer(trigger.message.author()).await?;
        if author.kind() != PeerKind::Human {
            return Ok(Vec::new());
        }
        let conversation = self
            .conversations
            .get(trigger.message.conversation_id())
            .await?
            .ok_or_else(|| {
                ProcessingError::ConversationNotFound(trigger.message.conversation_id())
            })?;
        let mut peers = Vec::new();
        let mut definitions = Vec::new();
        for participant in conversation.participants() {
            let peer = self.peer(participant.peer_id()).await?;
            if peer.kind() == PeerKind::Agent
                && let Some(definition) = self.agents.get(peer.id()).await?
            {
                if definition.peer_id() != peer.id() {
                    return Err(ProcessingError::InvalidDefinition(peer.id()));
                }
                definitions.push(definition);
            }
            peers.push(peer);
        }
        let selected = responding_agents(&conversation, &trigger.message, &author, &definitions);
        let pending = selected
            .into_iter()
            .filter(|peer| !self.completed.contains(&(event.id, *peer)))
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return Ok(Vec::new());
        }
        let entries = self
            .messages
            .history_through(conversation.id(), trigger.message.id())
            .await?
            .ok_or_else(|| ProcessingError::MessageNotFound(trigger.message.id()))?;
        let history = entries
            .into_iter()
            .map(|entry| entry.message)
            .collect::<Vec<_>>();
        for message in &history {
            if !peers.iter().any(|peer| peer.id() == message.author()) {
                peers.push(self.peer(message.author()).await?);
            }
        }
        let mut replies = Vec::new();
        for peer in pending {
            let definition = definitions
                .iter()
                .find(|definition| definition.peer_id() == peer)
                .ok_or(ProcessingError::InvalidDefinition(peer))?;
            let text = self
                .runtime
                .generate(definition, &conversation, &peers, &history)
                .await?;
            let reply = self
                .sender
                .execute(SendMessageRequest {
                    conversation_id: conversation.id(),
                    author: peer,
                    content: MessageContent::Text(text),
                    addressed_peers: vec![author.id()],
                    reply_to: Some(trigger.message.id()),
                })
                .await?;
            self.completed.insert((event.id, peer));
            replies.push(reply);
        }
        Ok(replies)
    }
}
