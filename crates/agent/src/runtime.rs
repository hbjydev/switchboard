//! Prompt projection and generation, without repository access or publication.
use crate::{
    AgentDefinition,
    model::{GenerationRequest, LanguageModel, ModelError, ParticipantContext, ProjectedMessage},
};
use std::sync::Arc;
use switchboard_kernel::{
    conversation::Conversation,
    message::Message,
    peer::{Peer, PeerKind},
};
use switchboard_uuids::{MessageId, PeerId};

pub struct AgentRuntime {
    model: Arc<dyn LanguageModel>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RuntimeError {
    #[error("peer {0} is missing from projection context")]
    MissingPeer(PeerId),
    #[error("agent {0} is not an eligible participant")]
    IneligibleAgent(PeerId),
    #[error("message {0} belongs to another conversation")]
    WrongConversation(MessageId),
    #[error(transparent)]
    Model(#[from] ModelError),
}

/// Projects messages without changing input history order.
///
/// The application supplies history through the trigger and resolves all current
/// participants and historical speakers.
/// Speakers who have since left the conversation still retain their identity.
pub fn project_request(
    definition: &AgentDefinition,
    conversation: &Conversation,
    peers: &[Peer],
    history: &[Message],
) -> Result<GenerationRequest, RuntimeError> {
    let acting_peer = definition.peer_id();
    let acting = find_peer(peers, acting_peer)?;
    if acting.kind() != PeerKind::Agent
        || !conversation
            .participant(acting_peer)
            .is_some_and(|p| p.role().can_send())
    {
        return Err(RuntimeError::IneligibleAgent(acting_peer));
    }
    let participants = conversation
        .participants()
        .iter()
        .map(|participant| {
            let peer = find_peer(peers, participant.peer_id())?;
            Ok(ParticipantContext {
                peer_id: peer.id(),
                display_name: peer.display_name().to_owned(),
                kind: peer.kind(),
                role: participant.role(),
            })
        })
        .collect::<Result<Vec<_>, RuntimeError>>()?;
    let messages = history
        .iter()
        .map(|message| {
            if message.conversation_id() != conversation.id() {
                return Err(RuntimeError::WrongConversation(message.id()));
            }
            let speaker = find_peer(peers, message.author())?;
            Ok(ProjectedMessage {
                message_id: message.id(),
                speaker: speaker.id(),
                display_name: speaker.display_name().to_owned(),
                content: message.content().clone(),
                is_acting_agent: speaker.id() == acting_peer,
            })
        })
        .collect::<Result<Vec<_>, RuntimeError>>()?;
    Ok(GenerationRequest {
        model: definition.model().clone(),
        acting_peer,
        instructions: definition.instructions().to_owned(),
        participants,
        messages,
    })
}

fn find_peer(peers: &[Peer], id: PeerId) -> Result<&Peer, RuntimeError> {
    peers
        .iter()
        .find(|peer| peer.id() == id)
        .ok_or(RuntimeError::MissingPeer(id))
}

impl AgentRuntime {
    #[must_use]
    pub fn new(model: Arc<dyn LanguageModel>) -> Self {
        Self { model }
    }

    /// Returns generated text; the application validates and publishes the reply.
    pub async fn generate(
        &self,
        definition: &AgentDefinition,
        conversation: &Conversation,
        peers: &[Peer],
        history: &[Message],
    ) -> Result<String, RuntimeError> {
        let request = project_request(definition, conversation, peers, history)?;
        Ok(self.model.generate(request).await?.text)
    }
}
