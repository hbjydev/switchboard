//! The kernel crate provides the core functionality for the application,
//! primarily in the form of domain models right now.

pub mod conversation;
pub mod event;
pub mod message;
pub mod peer;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DomainError {
    #[error("display name must not be blank")]
    BlankDisplayName,
    #[error("message text must not be blank")]
    BlankMessage,
}
