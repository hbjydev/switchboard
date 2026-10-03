use switchboard_uuids::{ConversationId, PeerId};

/// Represents a conversation between multiple participants.
#[derive(Debug, Clone)]
pub struct Conversation {
    id: ConversationId,
    participants: Vec<Participant>,
}

/// Represents a participant in a conversation, identified by their `PeerId` and
/// their role within the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    #[must_use]
    pub fn new(participants: impl IntoIterator<Item = PeerId>) -> Self {
        let mut conversation = Self {
            id: ConversationId::new(),
            participants: Vec::new(),
        };
        for peer in participants {
            conversation.add_participant(peer, ParticipantRole::Member);
        }
        conversation
    }

    #[must_use]
    pub const fn id(&self) -> ConversationId {
        self.id
    }

    #[must_use]
    pub fn participants(&self) -> &[Participant] {
        &self.participants
    }

    #[must_use]
    pub fn participant(&self, peer: PeerId) -> Option<&Participant> {
        self.participants
            .iter()
            .find(|participant| participant.peer_id == peer)
    }

    /// Checks if a participant with the given `PeerId` is part of the
    /// conversation.
    #[must_use]
    pub fn contains(&self, peer: PeerId) -> bool {
        self.participants.iter().any(|p| p.peer_id == peer)
    }

    /// Adds a participant if absent. Repeated additions preserve the existing role.
    pub fn add_participant(&mut self, peer: PeerId, role: ParticipantRole) {
        if !self.contains(peer) {
            self.participants.push(Participant {
                peer_id: peer,
                role,
            });
        }
    }

    /// Removes a participant from the conversation if they are present.
    pub fn remove_participant(&mut self, peer: PeerId) {
        self.participants.retain(|p| p.peer_id != peer);
    }
}

impl Participant {
    #[must_use]
    pub const fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    #[must_use]
    pub const fn role(&self) -> ParticipantRole {
        self.role
    }
}

impl ParticipantRole {
    #[must_use]
    pub const fn can_send(self) -> bool {
        matches!(self, Self::Member | Self::Moderator)
    }
}
