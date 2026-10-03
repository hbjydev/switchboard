#![cfg(feature = "mocks")]
#![expect(
    clippy::panic_in_result_fn,
    reason = "tests assert results while propagating setup failures"
)]
use std::sync::Arc;
use switchboard_application::{
    messaging::{SendMessage, SendMessageError, SendMessageRequest},
    repository::{
        MockConversationRepository, MockMessageRepository, MockPeerRepository, RepositoryError,
    },
};
use switchboard_kernel::{
    conversation::{Conversation, ParticipantRole},
    message::MessageContent,
    peer::{Peer, PeerKind},
};

#[tokio::test]
async fn moderator_send_reports_append_failure_without_returning_an_event()
-> Result<(), Box<dyn std::error::Error>> {
    for failure in [
        RepositoryError::Unavailable,
        RepositoryError::Conflict,
        RepositoryError::InvalidData,
    ] {
        let peer = Peer::new("moderator", PeerKind::Service)?;
        let author = peer.id();
        let mut conversation = Conversation::new([]);
        conversation.add_participant(author, ParticipantRole::Moderator);
        let id = conversation.id();
        let mut peers = MockPeerRepository::new();
        peers
            .expect_get()
            .with(mockall::predicate::eq(author))
            .times(1)
            .returning(move |_| Ok(Some(peer.clone())));
        let mut conversations = MockConversationRepository::new();
        conversations
            .expect_get()
            .with(mockall::predicate::eq(id))
            .times(1)
            .returning(move |_| Ok(Some(conversation.clone())));
        let mut messages = MockMessageRepository::new();
        messages
            .expect_append()
            .withf(move |entry| {
                entry.message.author() == author
                    && entry.message.conversation_id() == id
                    && entry.event.sender == author
            })
            .times(1)
            .returning(move |_| Err(failure));
        let service =
            SendMessage::new(Arc::new(peers), Arc::new(conversations), Arc::new(messages));
        let result = service
            .execute(SendMessageRequest {
                conversation_id: id,
                author,
                content: MessageContent::Text("hello".into()),
                addressed_peers: vec![],
                reply_to: None,
            })
            .await;
        assert!(matches!(result, Err(SendMessageError::Repository(error)) if error == failure));
    }
    Ok(())
}
