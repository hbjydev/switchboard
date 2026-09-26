use switchboard_uuids::{ConversationId, MessageId, PeerId};

pub enum DomainEvent {
    MessageCreated(MessageCreatedEvent),
    PeerJoinedConversation(PeerJoinedConversationEvent),
}

pub struct MessageCreatedEvent {
    pub message_id: MessageId,
    pub conversation_id: ConversationId,
    pub author: PeerId,
}

pub struct PeerJoinedConversationEvent {
    pub peer_id: PeerId,
    pub conversation_id: ConversationId,
}
