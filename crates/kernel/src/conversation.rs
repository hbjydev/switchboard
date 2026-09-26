use switchboard_uuids::{ConversationId, PeerId};

/// Represents a conversation between multiple participants.
pub struct Conversation {
    id: ConversationId,
    participants: Vec<Participant>,
}

/// Represents a participant in a conversation, identified by their `PeerId` and
/// their role within the conversation.
pub struct Participant {
    peer_id: PeerId,
    role: ParticipantRole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParticipantRole {
    Member,
    Observer,
    Moderator,
}

impl Conversation {
    /// Creates a new conversation with the given participants. Each participant
    /// is assigned the `Member` role by default.
    pub fn new(participants: impl IntoIterator<Item = PeerId>) -> Self {
        Self {
            id: ConversationId::new(),
            participants: participants
                .into_iter()
                .map(|peer_id| Participant {
                    peer_id,
                    role: ParticipantRole::Member,
                })
                .collect(),
        }
    }

    /// Checks if a participant with the given `PeerId` is part of the
    /// conversation.
    pub fn contains(&self, peer: PeerId) -> bool {
        self.participants.iter().any(|p| p.peer_id == peer)
    }

    /// Adds a participant to the conversation if they are not already present.
    pub fn add_participant(&mut self, peer: PeerId, role: ParticipantRole) {
        if !self.contains(peer) {
            self.participants.push(Participant { peer_id: peer, role });
        }
    }

    /// Removes a participant from the conversation if they are present.
    pub fn remove_participant(&mut self, peer: PeerId) {
        self.participants.retain(|p| p.peer_id != peer);
    }
}
