//! Validated message publication shared by humans, agents, and services.

use crate::repository::{
    ConversationRepository, MessageRepository, PeerRepository, RepositoryError, StoredMessage,
};
use std::sync::Arc;
use switchboard_kernel::{
    DomainError,
    event::{DomainEvent, Envelope, MessageCreatedEvent},
    message::{Message, MessageContent},
};
use switchboard_uuids::{ConversationId, MessageId, PeerId};

pub struct SendMessage {
    peers: Arc<dyn PeerRepository>,
    conversations: Arc<dyn ConversationRepository>,
    messages: Arc<dyn MessageRepository>,
}

#[derive(Debug, Clone)]
pub struct SendMessageRequest {
    pub conversation_id: ConversationId,
    pub author: PeerId,
    pub content: MessageContent,
    pub addressed_peers: Vec<PeerId>,
    /// The stored triggering message. Its retained event supplies provenance.
    pub reply_to: Option<MessageId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SendMessageError {
    #[error("conversation {0} does not exist")]
    ConversationNotFound(ConversationId),
    #[error("author {0} does not exist")]
    AuthorNotFound(PeerId),
    #[error("author {0} is not a conversation participant")]
    NotParticipant(PeerId),
    #[error("author {0} cannot send messages in this conversation")]
    CannotSend(PeerId),
    #[error("addressed peer {0} is not a conversation participant")]
    InvalidRecipient(PeerId),
    #[error("triggering message {0} does not exist")]
    TriggerNotFound(MessageId),
    #[error("triggering message {0} belongs to another conversation")]
    TriggerOutsideConversation(MessageId),
    #[error(transparent)]
    Domain(#[from] DomainError),
    #[error(transparent)]
    Repository(#[from] RepositoryError),
}

impl SendMessage {
    #[must_use]
    pub fn new(
        peers: Arc<dyn PeerRepository>,
        conversations: Arc<dyn ConversationRepository>,
        messages: Arc<dyn MessageRepository>,
    ) -> Self {
        Self {
            peers,
            conversations,
            messages,
        }
    }

    /// Returns the message and committed event only after a successful append.
    /// Membership reads and append are not a cross-repository transaction.
    pub async fn execute(
        &self,
        request: SendMessageRequest,
    ) -> Result<StoredMessage, SendMessageError> {
        let conversation = self
            .conversations
            .get(request.conversation_id)
            .await?
            .ok_or(SendMessageError::ConversationNotFound(
                request.conversation_id,
            ))?;
        self.peers
            .get(request.author)
            .await?
            .ok_or(SendMessageError::AuthorNotFound(request.author))?;
        let participant = conversation
            .participant(request.author)
            .ok_or(SendMessageError::NotParticipant(request.author))?;
        if !participant.role().can_send() {
            return Err(SendMessageError::CannotSend(request.author));
        }
        let message = Message::new(
            request.conversation_id,
            request.author,
            request.content,
            request.addressed_peers,
        )?;
        for recipient in message.addressed_peers() {
            if !conversation.contains(*recipient) {
                return Err(SendMessageError::InvalidRecipient(*recipient));
            }
        }
        let payload = DomainEvent::MessageCreated(MessageCreatedEvent {
            message_id: message.id(),
            conversation_id: message.conversation_id(),
            author: message.author(),
        });
        let event = if let Some(id) = request.reply_to {
            let trigger = self
                .messages
                .get(id)
                .await?
                .ok_or(SendMessageError::TriggerNotFound(id))?;
            if trigger.message.conversation_id() != message.conversation_id() {
                return Err(SendMessageError::TriggerOutsideConversation(id));
            }
            Envelope::reply(message.author(), payload, &trigger.event)
        } else {
            Envelope::root(message.author(), payload)
        };
        let stored = StoredMessage { message, event };
        self.messages.append(stored.clone()).await?;
        Ok(stored)
    }
}
