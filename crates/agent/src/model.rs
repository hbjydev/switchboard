//! Provider-independent generation port. Provider roles belong in adapters.
use crate::ModelRef;
use async_trait::async_trait;
use switchboard_kernel::{conversation::ParticipantRole, message::MessageContent, peer::PeerKind};
use switchboard_uuids::{MessageId, PeerId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParticipantContext {
    pub peer_id: PeerId,
    pub display_name: String,
    pub kind: PeerKind,
    pub role: ParticipantRole,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectedMessage {
    pub message_id: MessageId,
    pub speaker: PeerId,
    pub display_name: String,
    pub content: MessageContent,
    pub is_acting_agent: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationRequest {
    pub model: ModelRef,
    pub acting_peer: PeerId,
    pub instructions: String,
    pub participants: Vec<ParticipantContext>,
    /// Ordered conversation data; role-like text never becomes instructions.
    pub messages: Vec<ProjectedMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationResponse {
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ModelError {
    #[error("model unavailable")]
    Unavailable,
    #[error("model request is invalid")]
    InvalidRequest,
    #[error("model generation timed out")]
    Timeout,
    #[error("model generation was cancelled")]
    Cancelled,
}

#[async_trait]
pub trait LanguageModel: Send + Sync {
    async fn generate(&self, request: GenerationRequest) -> Result<GenerationResponse, ModelError>;
}
