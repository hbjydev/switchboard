use chrono::{DateTime, Utc};
use switchboard_uuids::{ConversationId, MessageId, PeerId};

#[derive(Debug, Clone)]
pub struct Message {
    id: MessageId,
    conversation_id: ConversationId,
    author: PeerId,
    content: MessageContent,
    addressed_peers: Vec<PeerId>,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageContent {
    Text(String),
}

impl Message {
    pub fn new(
        conversation_id: ConversationId,
        author: PeerId,
        content: MessageContent,
        addressed_peers: impl IntoIterator<Item = PeerId>,
    ) -> Result<Self, crate::DomainError> {
        let MessageContent::Text(text) = &content;
        if text.trim().is_empty() {
            return Err(crate::DomainError::BlankMessage);
        }
        let mut recipients = Vec::new();
        for peer in addressed_peers {
            if !recipients.contains(&peer) {
                recipients.push(peer);
            }
        }
        Ok(Self {
            id: MessageId::new(),
            conversation_id,
            author,
            content,
            addressed_peers: recipients,
            created_at: Utc::now(),
        })
    }

    #[must_use]
    pub const fn id(&self) -> MessageId {
        self.id
    }

    #[must_use]
    pub const fn conversation_id(&self) -> ConversationId {
        self.conversation_id
    }

    #[must_use]
    pub const fn author(&self) -> PeerId {
        self.author
    }

    #[must_use]
    pub const fn content(&self) -> &MessageContent {
        &self.content
    }

    #[must_use]
    pub fn addressed_peers(&self) -> &[PeerId] {
        &self.addressed_peers
    }

    #[must_use]
    pub const fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }
}
