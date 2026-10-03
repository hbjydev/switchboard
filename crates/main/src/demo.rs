//! Composition for the local demo, executed through the switchboard binary.
use anyhow::{Context, Result};
use std::{fmt::Write, sync::Arc};
use switchboard_agent::{AgentDefinition, ModelRef, runtime::AgentRuntime};
use switchboard_application::{
    messaging::{SendMessage, SendMessageRequest},
    processing::ActivationProcessor,
    repository::{AgentRepository, ConversationRepository, MessageRepository, PeerRepository},
};
use switchboard_infrastructure::{
    fake_model::FakeModel,
    memory::{
        InMemoryAgentRepository, InMemoryConversationRepository, InMemoryMessageRepository,
        InMemoryPeerRepository,
    },
};
use switchboard_kernel::{
    conversation::Conversation,
    message::MessageContent,
    peer::{Peer, PeerKind},
};

pub async fn run(text: String) -> Result<String> {
    let peers = Arc::new(InMemoryPeerRepository::default());
    let conversations = Arc::new(InMemoryConversationRepository::default());
    let messages = Arc::new(InMemoryMessageRepository::default());
    let agents = Arc::new(InMemoryAgentRepository::default());
    let human = Peer::new("Human", PeerKind::Human)?;
    let agent = Peer::new("Agent", PeerKind::Agent)?;
    peers.save(human.clone()).await?;
    peers.save(agent.clone()).await?;
    // Trusted demo setup uses the peers just registered above.
    let conversation = Conversation::new([human.id(), agent.id()]);
    conversations.save(conversation.clone()).await?;
    agents
        .save(AgentDefinition::new(
            &agent,
            "Respond helpfully to the conversation.",
            ModelRef {
                provider: "fake".into(),
                model: "deterministic".into(),
            },
        )?)
        .await?;
    let sender = SendMessage::new(peers.clone(), conversations.clone(), messages.clone());
    let mut processor = ActivationProcessor::new(
        peers.clone(),
        conversations,
        messages.clone(),
        agents,
        AgentRuntime::new(Arc::new(FakeModel::default())),
    );
    let sent = sender
        .execute(SendMessageRequest {
            conversation_id: conversation.id(),
            author: human.id(),
            content: MessageContent::Text(text),
            addressed_peers: vec![],
            reply_to: None,
        })
        .await
        .context("sending demo message")?;
    processor
        .process(&sent.event)
        .await
        .context("processing demo activation")?;
    let mut transcript = String::new();
    for entry in messages.history(conversation.id()).await? {
        let peer = peers
            .get(entry.message.author())
            .await?
            .context("stored message author is missing")?;
        let MessageContent::Text(content) = entry.message.content();
        writeln!(
            transcript,
            "{} [{}]: {content}",
            peer.display_name(),
            peer.id()
        )?;
    }
    Ok(transcript)
}
