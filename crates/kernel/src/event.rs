use chrono::{DateTime, Utc};
use switchboard_uuids::{ConversationId, EventId, MessageId, PeerId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    pub id: EventId,
    pub causation: Option<EventId>,
    pub correlation: EventId,
    pub sender: PeerId,
    pub payload: DomainEvent,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DomainEvent {
    MessageCreated(MessageCreatedEvent),
    PeerJoinedConversation(PeerJoinedConversationEvent),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageCreatedEvent {
    pub message_id: MessageId,
    pub conversation_id: ConversationId,
    pub author: PeerId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerJoinedConversationEvent {
    pub peer_id: PeerId,
    pub conversation_id: ConversationId,
}

impl Envelope {
    #[must_use]
    pub fn root(sender: PeerId, payload: DomainEvent) -> Self {
        let id = EventId::new();
        Self {
            id,
            sender,
            payload,
            timestamp: Utc::now(),
            causation: None,
            correlation: id,
        }
    }

    #[must_use]
    pub fn reply(sender: PeerId, payload: DomainEvent, trigger: &Self) -> Self {
        Self {
            id: EventId::new(),
            sender,
            payload,
            timestamp: Utc::now(),
            causation: Some(trigger.id),
            correlation: trigger.correlation,
        }
    }
}
