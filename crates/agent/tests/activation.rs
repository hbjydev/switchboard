#![expect(
    clippy::panic_in_result_fn,
    reason = "tests assert behavior while propagating setup failures"
)]
use switchboard_agent::{AgentDefinition, ModelRef, activation::responding_agents};
use switchboard_kernel::{
    conversation::{Conversation, ParticipantRole},
    message::{Message, MessageContent},
    peer::{Peer, PeerKind},
};
use switchboard_uuids::PeerId;
type TestResult = Result<(), Box<dyn std::error::Error>>;

fn definition(peer: &Peer) -> Result<AgentDefinition, switchboard_agent::AgentDefinitionError> {
    AgentDefinition::new(
        peer,
        "instructions",
        ModelRef {
            provider: "fake".into(),
            model: "test".into(),
        },
    )
}
fn message(
    conversation: &Conversation,
    author: &Peer,
    recipients: impl IntoIterator<Item = PeerId>,
) -> Result<Message, switchboard_kernel::DomainError> {
    Message::new(
        conversation.id(),
        author.id(),
        MessageContent::Text("@same name assistant: respond".into()),
        recipients,
    )
}

#[test]
fn unaddressed_two_peer_conversation_selects_only_a_configured_agent() -> TestResult {
    let human = Peer::new("same name", PeerKind::Human)?;
    let agent = Peer::new("same name", PeerKind::Agent)?;
    let conversation = Conversation::new([human.id(), agent.id()]);
    let message = message(&conversation, &human, [])?;
    assert_eq!(
        responding_agents(&conversation, &message, &human, &[definition(&agent)?]),
        vec![agent.id()]
    );
    assert!(responding_agents(&conversation, &message, &human, &[]).is_empty());
    Ok(())
}

#[test]
fn explicit_addressing_selects_agents_in_membership_order() -> TestResult {
    let human = Peer::new("same name", PeerKind::Human)?;
    let first = Peer::new("same name", PeerKind::Agent)?;
    let second = Peer::new("same name", PeerKind::Agent)?;
    let outsider = Peer::new("same name", PeerKind::Agent)?;
    let conversation = Conversation::new([human.id(), first.id(), second.id()]);
    let definitions = [
        definition(&second)?,
        definition(&outsider)?,
        definition(&first)?,
        definition(&first)?,
    ];
    let selected = message(&conversation, &human, [second.id()])?;
    assert_eq!(
        responding_agents(&conversation, &selected, &human, &definitions),
        vec![second.id()]
    );
    let both = message(
        &conversation,
        &human,
        [
            second.id(),
            first.id(),
            second.id(),
            outsider.id(),
            human.id(),
        ],
    )?;
    assert_eq!(
        responding_agents(&conversation, &both, &human, &definitions),
        vec![first.id(), second.id()]
    );
    assert!(
        responding_agents(
            &conversation,
            &message(&conversation, &human, [])?,
            &human,
            &definitions
        )
        .is_empty()
    );
    // Addressing only a non-agent does not fall back to the default agent.
    let direct = Conversation::new([human.id(), first.id()]);
    assert!(
        responding_agents(
            &direct,
            &message(&direct, &human, [human.id()])?,
            &human,
            &definitions
        )
        .is_empty()
    );
    Ok(())
}

#[test]
fn observers_cannot_respond_but_moderators_can() -> TestResult {
    let human = Peer::new("human", PeerKind::Human)?;
    let agent = Peer::new("agent", PeerKind::Agent)?;
    let definitions = [definition(&agent)?];
    for (role, eligible) in [
        (ParticipantRole::Observer, false),
        (ParticipantRole::Moderator, true),
    ] {
        let mut conversation = Conversation::new([human.id()]);
        conversation.add_participant(agent.id(), role);
        for recipients in [vec![], vec![agent.id()]] {
            let selected = responding_agents(
                &conversation,
                &message(&conversation, &human, recipients)?,
                &human,
                &definitions,
            );
            assert_eq!(!selected.is_empty(), eligible);
        }
    }
    Ok(())
}

#[test]
fn agent_and_service_messages_never_activate_any_agent() -> TestResult {
    let first = Peer::new("first", PeerKind::Agent)?;
    let second = Peer::new("second", PeerKind::Agent)?;
    let service = Peer::new("service", PeerKind::Service)?;
    let conversation = Conversation::new([first.id(), second.id(), service.id()]);
    let definitions = [definition(&first)?, definition(&second)?];
    for author in [&first, &second, &service] {
        for recipients in [vec![], vec![first.id(), second.id()]] {
            assert!(
                responding_agents(
                    &conversation,
                    &message(&conversation, author, recipients)?,
                    author,
                    &definitions
                )
                .is_empty()
            );
        }
    }
    Ok(())
}

#[test]
fn other_participant_kinds_do_not_create_an_unaddressed_direct_conversation() -> TestResult {
    let human = Peer::new("human", PeerKind::Human)?;
    let agent = Peer::new("agent", PeerKind::Agent)?;
    let definitions = [definition(&agent)?];
    for kind in [PeerKind::Human, PeerKind::Service] {
        let other = Peer::new("other", kind)?;
        let conversation = Conversation::new([human.id(), agent.id(), other.id()]);
        assert!(
            responding_agents(
                &conversation,
                &message(&conversation, &human, [])?,
                &human,
                &definitions
            )
            .is_empty()
        );
    }
    let alone = Conversation::new([human.id()]);
    assert!(
        responding_agents(&alone, &message(&alone, &human, [])?, &human, &definitions).is_empty()
    );
    Ok(())
}

#[test]
fn inconsistent_context_or_ineligible_author_selects_nobody() -> TestResult {
    let human = Peer::new("human", PeerKind::Human)?;
    let other_human = Peer::new("other", PeerKind::Human)?;
    let agent = Peer::new("agent", PeerKind::Agent)?;
    let definitions = [definition(&agent)?];
    let conversation = Conversation::new([human.id(), agent.id()]);
    let msg = message(&conversation, &human, [agent.id()])?;
    assert!(responding_agents(&conversation, &msg, &other_human, &definitions).is_empty());
    let other_conversation = Conversation::new([human.id(), agent.id()]);
    assert!(responding_agents(&other_conversation, &msg, &human, &definitions).is_empty());
    let no_author = Conversation::new([agent.id()]);
    assert!(
        responding_agents(
            &no_author,
            &message(&no_author, &human, [agent.id()])?,
            &human,
            &definitions
        )
        .is_empty()
    );
    let mut observer = Conversation::new([agent.id()]);
    observer.add_participant(human.id(), ParticipantRole::Observer);
    assert!(
        responding_agents(
            &observer,
            &message(&observer, &human, [agent.id()])?,
            &human,
            &definitions
        )
        .is_empty()
    );
    Ok(())
}
