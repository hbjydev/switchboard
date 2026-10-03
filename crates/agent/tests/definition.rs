#![expect(
    clippy::panic_in_result_fn,
    reason = "tests assert behavior while propagating setup errors"
)]

use switchboard_agent::{AgentDefinition, AgentDefinitionError, ModelRef};
use switchboard_kernel::{
    DomainError,
    peer::{Peer, PeerKind},
};

#[test]
fn definitions_require_agent_peers() -> Result<(), DomainError> {
    for kind in [PeerKind::Human, PeerKind::Service] {
        let peer = Peer::new("same name", kind)?;
        let result = AgentDefinition::new(&peer, "instructions", model());
        assert!(matches!(result, Err(AgentDefinitionError::NotAgent(id)) if id == peer.id()));
    }
    Ok(())
}

#[test]
fn definitions_preserve_identity_and_configuration() -> Result<(), Box<dyn std::error::Error>> {
    let peer = Peer::new("same name", PeerKind::Agent)?;
    let definition = AgentDefinition::new(&peer, " instructions\n", model())?;
    assert_eq!(definition.peer_id(), peer.id());
    assert_eq!(definition.instructions(), " instructions\n");
    assert_eq!(definition.model(), &model());
    Ok(())
}

fn model() -> ModelRef {
    ModelRef {
        provider: "fake".into(),
        model: "deterministic".into(),
    }
}
