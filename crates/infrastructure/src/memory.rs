//! In-memory repositories. Each operation holds one short synchronous lock;
//! no lock is held across an await. Poisoned locks report unavailability.

use async_trait::async_trait;
use std::{
    collections::{HashMap, HashSet},
    sync::{Mutex, MutexGuard},
};
use switchboard_agent::AgentDefinition;
use switchboard_application::repository::{
    AgentRepository, ConversationRepository, MessageRepository, PeerRepository, RepositoryError,
    StoredMessage,
};
use switchboard_kernel::{
    conversation::Conversation,
    event::DomainEvent,
    peer::{Peer, PeerAlias},
};
use switchboard_uuids::{ConversationId, EventId, MessageId, PeerId};

fn lock<T>(state: &Mutex<T>) -> Result<MutexGuard<'_, T>, RepositoryError> {
    state
        .lock()
        .map_err(|_poisoned| RepositoryError::Unavailable)
}

#[derive(Default)]
struct PeerState {
    peers: HashMap<PeerId, Peer>,
    aliases: HashMap<PeerAlias, PeerId>,
}

#[derive(Default)]
pub struct InMemoryPeerRepository {
    state: Mutex<PeerState>,
}

#[async_trait]
impl PeerRepository for InMemoryPeerRepository {
    async fn get(&self, id: PeerId) -> Result<Option<Peer>, RepositoryError> {
        Ok(lock(&self.state)?.peers.get(&id).cloned())
    }

    async fn resolve_alias(&self, alias: &PeerAlias) -> Result<Option<Peer>, RepositoryError> {
        let state = lock(&self.state)?;
        Ok(state
            .aliases
            .get(alias)
            .and_then(|id| state.peers.get(id))
            .cloned())
    }

    async fn save(&self, peer: Peer) -> Result<(), RepositoryError> {
        let mut state = lock(&self.state)?;
        let id = peer.id();
        if peer
            .aliases()
            .iter()
            .any(|alias| state.aliases.get(alias).is_some_and(|owner| *owner != id))
        {
            return Err(RepositoryError::Conflict);
        }
        state.aliases.retain(|_, owner| *owner != id);
        for alias in peer.aliases() {
            state.aliases.insert(alias.clone(), id);
        }
        state.peers.insert(id, peer);
        drop(state);
        Ok(())
    }
}

#[derive(Default)]
pub struct InMemoryConversationRepository {
    state: Mutex<HashMap<ConversationId, Conversation>>,
}

#[async_trait]
impl ConversationRepository for InMemoryConversationRepository {
    async fn get(&self, id: ConversationId) -> Result<Option<Conversation>, RepositoryError> {
        Ok(lock(&self.state)?.get(&id).cloned())
    }

    async fn save(&self, conversation: Conversation) -> Result<(), RepositoryError> {
        lock(&self.state)?.insert(conversation.id(), conversation);
        Ok(())
    }
}

#[derive(Default)]
pub struct InMemoryAgentRepository {
    state: Mutex<HashMap<PeerId, AgentDefinition>>,
}

#[async_trait]
impl AgentRepository for InMemoryAgentRepository {
    async fn get(&self, peer: PeerId) -> Result<Option<AgentDefinition>, RepositoryError> {
        Ok(lock(&self.state)?.get(&peer).cloned())
    }

    async fn save(&self, definition: AgentDefinition) -> Result<(), RepositoryError> {
        lock(&self.state)?.insert(definition.peer_id(), definition);
        Ok(())
    }
}

#[derive(Default)]
struct MessageState {
    entries: HashMap<MessageId, StoredMessage>,
    events: HashSet<EventId>,
    histories: HashMap<ConversationId, Vec<MessageId>>,
}

#[derive(Default)]
pub struct InMemoryMessageRepository {
    state: Mutex<MessageState>,
}

fn validate(entry: &StoredMessage) -> Result<(), RepositoryError> {
    let DomainEvent::MessageCreated(payload) = &entry.event.payload else {
        return Err(RepositoryError::InvalidData);
    };
    if payload.message_id != entry.message.id()
        || payload.conversation_id != entry.message.conversation_id()
        || payload.author != entry.message.author()
        || entry.event.sender != entry.message.author()
        || (entry.event.causation.is_none() && entry.event.correlation != entry.event.id)
        || entry.event.causation == Some(entry.event.id)
    {
        return Err(RepositoryError::InvalidData);
    }
    Ok(())
}

fn history_entries(
    state: &MessageState,
    ids: impl Iterator<Item = MessageId>,
) -> Result<Vec<StoredMessage>, RepositoryError> {
    ids.map(|id| {
        state
            .entries
            .get(&id)
            .cloned()
            .ok_or(RepositoryError::InvalidData)
    })
    .collect()
}

#[async_trait]
impl MessageRepository for InMemoryMessageRepository {
    async fn append(&self, entry: StoredMessage) -> Result<(), RepositoryError> {
        validate(&entry)?;
        let mut state = lock(&self.state)?;
        let id = entry.message.id();
        if state.entries.contains_key(&id) || state.events.contains(&entry.event.id) {
            return Err(RepositoryError::Conflict);
        }
        state
            .histories
            .entry(entry.message.conversation_id())
            .or_default()
            .push(id);
        state.events.insert(entry.event.id);
        state.entries.insert(id, entry);
        drop(state);
        Ok(())
    }

    async fn get(&self, id: MessageId) -> Result<Option<StoredMessage>, RepositoryError> {
        Ok(lock(&self.state)?.entries.get(&id).cloned())
    }

    async fn history(
        &self,
        conversation: ConversationId,
    ) -> Result<Vec<StoredMessage>, RepositoryError> {
        let state = lock(&self.state)?;
        history_entries(
            &state,
            state
                .histories
                .get(&conversation)
                .into_iter()
                .flatten()
                .copied(),
        )
    }

    async fn history_through(
        &self,
        conversation: ConversationId,
        through: MessageId,
    ) -> Result<Option<Vec<StoredMessage>>, RepositoryError> {
        let state = lock(&self.state)?;
        let Some(ids) = state.histories.get(&conversation) else {
            return Ok(None);
        };
        let Some(position) = ids.iter().position(|id| *id == through) else {
            return Ok(None);
        };
        let result = history_entries(&state, ids.iter().take(position + 1).copied());
        drop(state);
        result.map(Some)
    }
}
