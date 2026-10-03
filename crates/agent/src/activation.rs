//! Deterministic response selection; receiving a message does not run an agent.

use crate::AgentDefinition;
use switchboard_kernel::{
    conversation::Conversation,
    message::Message,
    peer::{Peer, PeerKind},
};
use switchboard_uuids::PeerId;

/// Selects configured agents allowed to speak, in conversation membership order.
///
/// Only human-authored messages can activate agents. Explicit addressing selects
/// exactly the eligible addressed agents. Without addressing, only a two-peer
/// conversation containing the human author and one configured agent activates.
/// Unknown definitions and observers are skipped; display names and text are
/// never interpreted as addressing. No observation or memory state is written.
///
/// Application code resolves the author and definitions from repositories. A
/// mismatched conversation/author or an author unable to speak selects nobody.
#[must_use]
pub fn responding_agents(
    conversation: &Conversation,
    message: &Message,
    author: &Peer,
    definitions: &[AgentDefinition],
) -> Vec<PeerId> {
    if message.conversation_id() != conversation.id()
        || message.author() != author.id()
        || author.kind() != PeerKind::Human
        || !conversation
            .participant(author.id())
            .is_some_and(|participant| participant.role().can_send())
    {
        return Vec::new();
    }
    let addressed = message.addressed_peers();
    if addressed.is_empty() && conversation.participants().len() != 2 {
        return Vec::new();
    }
    conversation
        .participants()
        .iter()
        .filter(|participant| {
            participant.peer_id() != author.id()
                && participant.role().can_send()
                && definitions
                    .iter()
                    .any(|definition| definition.peer_id() == participant.peer_id())
                && (addressed.is_empty() || addressed.contains(&participant.peer_id()))
        })
        .map(switchboard_kernel::conversation::Participant::peer_id)
        .collect()
}
