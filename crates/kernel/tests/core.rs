#![expect(
    clippy::panic_in_result_fn,
    reason = "tests use assertions while propagating setup failures"
)]

use switchboard_kernel::{
    DomainError,
    conversation::{Conversation, Participant, ParticipantRole},
    event::{DomainEvent, Envelope, MessageCreatedEvent},
    message::{Message, MessageContent},
    peer::{Peer, PeerAlias, PeerKind},
};
use switchboard_uuids::{ConversationId, MessageId, PeerId};

#[test]
fn peer_names_are_validated_on_creation_and_rename() -> Result<(), DomainError> {
    assert!(matches!(
        Peer::new(" \n\t", PeerKind::Human),
        Err(DomainError::BlankDisplayName)
    ));
    let mut peer = Peer::new(" Alice ", PeerKind::Human)?;
    assert_eq!(peer.display_name(), " Alice ");
    assert_eq!(peer.rename(" "), Err(DomainError::BlankDisplayName));
    assert_eq!(peer.display_name(), " Alice ");
    peer.rename("Bob")?;
    assert_eq!(peer.display_name(), "Bob");
    Ok(())
}

#[test]
fn aliases_use_the_complete_scoped_identity() -> Result<(), DomainError> {
    let alias = PeerAlias {
        provider: "matrix".into(),
        scope: "account-a".into(),
        external_id: "alice".into(),
    };
    let mut peer = Peer::new("Alice", PeerKind::Human)?;
    peer.add_alias(alias.clone());
    peer.add_alias(alias.clone());
    let other_scope = PeerAlias {
        scope: "account-b".into(),
        ..alias.clone()
    };
    let other_provider = PeerAlias {
        provider: "slack".into(),
        ..alias
    };
    peer.add_alias(other_scope);
    peer.add_alias(other_provider);
    assert_eq!(peer.aliases().len(), 3);
    Ok(())
}

#[test]
fn membership_is_unique_and_repeated_additions_preserve_roles() {
    let peer = PeerId::new();
    let observer = PeerId::new();
    let mut conversation = Conversation::new([peer, peer]);
    assert_eq!(conversation.participants().len(), 1);
    conversation.add_participant(observer, ParticipantRole::Observer);
    conversation.add_participant(observer, ParticipantRole::Moderator);
    assert_eq!(
        conversation.participant(observer).map(Participant::role),
        Some(ParticipantRole::Observer)
    );
    assert!(ParticipantRole::Member.can_send());
    assert!(ParticipantRole::Moderator.can_send());
    assert!(!ParticipantRole::Observer.can_send());
    conversation.remove_participant(observer);
    conversation.remove_participant(observer);
    assert!(!conversation.contains(observer));
    assert_eq!(conversation.participants().len(), 1);
}

#[test]
fn messages_reject_blank_text_and_preserve_content_and_recipients() -> Result<(), DomainError> {
    let conversation = ConversationId::new();
    let author = PeerId::new();
    let recipient = PeerId::new();
    assert!(matches!(
        Message::new(conversation, author, MessageContent::Text(" \n".into()), []),
        Err(DomainError::BlankMessage)
    ));
    let content = MessageContent::Text("  hello\n".into());
    let message = Message::new(
        conversation,
        author,
        content.clone(),
        [recipient, recipient],
    )?;
    assert_eq!(message.content(), &content);
    assert_eq!(message.author(), author);
    assert_eq!(message.conversation_id(), conversation);
    assert_eq!(message.addressed_peers(), &[recipient]);
    Ok(())
}

#[test]
fn replies_inherit_root_correlation_and_immediate_causation() {
    let author = PeerId::new();
    let payload = DomainEvent::MessageCreated(MessageCreatedEvent {
        message_id: MessageId::new(),
        conversation_id: ConversationId::new(),
        author,
    });
    let root = Envelope::root(author, payload.clone());
    assert_eq!(root.causation, None);
    assert_eq!(root.correlation, root.id);
    let reply = Envelope::reply(author, payload.clone(), &root);
    let next = Envelope::reply(author, payload, &reply);
    assert_ne!(root.id, reply.id);
    assert_eq!(reply.causation, Some(root.id));
    assert_eq!(next.causation, Some(reply.id));
    assert_eq!(next.correlation, root.id);
}
