//! Agent configuration, independent of the runtime that executes it.

use switchboard_kernel::peer::{Peer, PeerKind};
use switchboard_uuids::PeerId;

/// A configured provider and model. Neither value implies a commercial default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRef {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Clone)]
pub struct AgentDefinition {
    peer_id: PeerId,
    instructions: String,
    model: ModelRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AgentDefinitionError {
    #[error("peer {0} is not an agent")]
    NotAgent(PeerId),
}

impl AgentDefinition {
    /// Requires a resolved agent peer; peer existence is checked by application code.
    pub fn new(
        peer: &Peer,
        instructions: impl Into<String>,
        model: ModelRef,
    ) -> Result<Self, AgentDefinitionError> {
        if peer.kind() != PeerKind::Agent {
            return Err(AgentDefinitionError::NotAgent(peer.id()));
        }
        Ok(Self {
            peer_id: peer.id(),
            instructions: instructions.into(),
            model,
        })
    }

    #[must_use]
    pub const fn peer_id(&self) -> PeerId {
        self.peer_id
    }

    #[must_use]
    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    #[must_use]
    pub const fn model(&self) -> &ModelRef {
        &self.model
    }
}

pub mod activation;

pub mod model;
pub mod runtime;
