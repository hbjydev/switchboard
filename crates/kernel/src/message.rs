use switchboard_uuids::{ConversationId, MessageId, PeerId};
use chrono::{DateTime, Utc};

pub struct Message {
    id: MessageId,
    conversation_id: ConversationId,
    author: PeerId,
    content: MessageContent,
    created_at: DateTime<Utc>,
}

pub enum MessageContent {
    Text(String),
}
