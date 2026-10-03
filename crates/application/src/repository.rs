//! Trusted storage boundaries for the first conversation slice.
//!
//! Ports are object-safe and support `Arc<dyn Trait>` across runtime threads.
//! Adapters translate backend failures into the shared error vocabulary. Lookup
//! absence is `Ok(None)`, distinct from a storage failure. Application services
//! validate peer existence, membership, and permission before writes.

use async_trait::async_trait;
use switchboard_agent::AgentDefinition;
use switchboard_kernel::{
    conversation::Conversation,
    event::Envelope,
    message::Message,
    peer::{Peer, PeerAlias},
};
use switchboard_uuids::{ConversationId, MessageId, PeerId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RepositoryError {
    #[error("repository unavailable")]
    Unavailable,
    #[error("repository write conflicts with existing data")]
    Conflict,
    #[error("repository data or input is invalid")]
    InvalidData,
}

#[cfg_attr(feature = "mocks", mockall::automock)]
#[async_trait]
pub trait PeerRepository: Send + Sync {
    async fn get(&self, id: PeerId) -> Result<Option<Peer>, RepositoryError>;

    /// Resolves the complete (provider, scope, external ID) tuple, never a name.
    async fn resolve_alias(&self, alias: &PeerAlias) -> Result<Option<Peer>, RepositoryError>;

    /// Inserts or replaces a peer, including its aliases.
    ///
    /// Alias ownership must be checked atomically with the write. An alias owned
    /// by another peer returns `Conflict` and leaves all stored data unchanged.
    /// Re-saving an alias for the same peer is permitted. Replacing a peer also
    /// replaces its alias lookup entries; no stale aliases may remain.
    async fn save(&self, peer: Peer) -> Result<(), RepositoryError>;
}

#[cfg_attr(feature = "mocks", mockall::automock)]
#[async_trait]
pub trait ConversationRepository: Send + Sync {
    async fn get(&self, id: ConversationId) -> Result<Option<Conversation>, RepositoryError>;

    /// Inserts or replaces a conversation. The application validates its peers.
    async fn save(&self, conversation: Conversation) -> Result<(), RepositoryError>;
}

/// A message and its retained provenance, stored and retrieved together.
#[derive(Debug, Clone)]
pub struct StoredMessage {
    pub message: Message,
    pub event: Envelope,
}

#[cfg_attr(feature = "mocks", mockall::automock)]
#[async_trait]
pub trait MessageRepository: Send + Sync {
    /// Appends a message and its `MessageCreated` envelope as one local write.
    ///
    /// The envelope must describe this message, with sender equal to author;
    /// mismatches return `InvalidData`. Duplicate message or event IDs return
    /// `Conflict`. Failure must leave neither half stored. Successful append
    /// fixes history position; timestamps must not be used to reorder it.
    /// Application code exposes the event for processing only after success.
    /// This is not an outbox or a guarantee of durable event delivery.
    async fn append(&self, entry: StoredMessage) -> Result<(), RepositoryError>;

    async fn get(&self, id: MessageId) -> Result<Option<StoredMessage>, RepositoryError>;

    /// Returns all entries in append order; an empty history returns an empty vec.
    async fn history(
        &self,
        conversation: ConversationId,
    ) -> Result<Vec<StoredMessage>, RepositoryError>;

    /// Returns the inclusive append-order prefix ending at `through`.
    ///
    /// Later arrivals must not enter the result. Returns `None` if the boundary
    /// message is absent or belongs to another conversation. An adapter must
    /// obtain a consistent prefix even while other callers append messages.
    async fn history_through(
        &self,
        conversation: ConversationId,
        through: MessageId,
    ) -> Result<Option<Vec<StoredMessage>>, RepositoryError>;
}

#[cfg_attr(feature = "mocks", mockall::automock)]
#[async_trait]
pub trait AgentRepository: Send + Sync {
    async fn get(&self, peer: PeerId) -> Result<Option<AgentDefinition>, RepositoryError>;

    /// Inserts or replaces the definition keyed by its agent `PeerId`.
    /// Application setup resolves the existing peer before constructing it.
    async fn save(&self, definition: AgentDefinition) -> Result<(), RepositoryError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    // Compilation guards the public ports' object safety and threading bounds.
    #[test]
    fn ports_can_be_shared_as_trait_objects() {
        fn assert_shareable<T: Send + Sync>() {}
        assert_shareable::<Arc<dyn PeerRepository>>();
        assert_shareable::<Arc<dyn ConversationRepository>>();
        assert_shareable::<Arc<dyn MessageRepository>>();
        assert_shareable::<Arc<dyn AgentRepository>>();
    }
}
