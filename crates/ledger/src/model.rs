use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, sqlx::Type)]
        #[sqlx(transparent)]
        pub struct $name(pub Uuid);
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
        impl std::str::FromStr for $name {
            type Err = uuid::Error;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                value.parse().map(Self)
            }
        }
    };
}
id_type!(IssueId);
id_type!(PeerId);
id_type!(AttemptId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "peer_kind")]
pub enum PeerKind {
    Human,
    Agent,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "issue_kind")]
pub enum IssueKind {
    Task,
    Question,
    Approval,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "issue_status")]
pub enum IssueStatus {
    Backlog,
    Ready,
    Claimed,
    Running,
    Blocked,
    WaitingForHuman,
    Completed,
    Failed,
    Cancelled,
}
impl IssueStatus {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Peer {
    pub id: PeerId,
    pub name: String,
    pub kind: PeerKind,
}
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Issue {
    pub id: IssueId,
    pub title: String,
    pub description: String,
    pub kind: IssueKind,
    pub status: IssueStatus,
    pub created_by: PeerId,
    pub owner: Option<PeerId>,
    pub current_attempt_id: Option<AttemptId>,
    pub parent_id: Option<IssueId>,
    pub priority: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone)]
pub struct NewIssue {
    pub title: String,
    pub description: String,
    pub kind: IssueKind,
    pub priority: i32,
    pub backlog: bool,
}
impl NewIssue {
    pub fn task(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            description: String::new(),
            kind: IssueKind::Task,
            priority: 0,
            backlog: false,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct IssueEvent {
    pub id: i64,
    pub issue_id: IssueId,
    pub event_type: String,
    pub actor: Option<PeerId>,
    pub occurred_at: DateTime<Utc>,
    pub metadata: serde_json::Value,
}

/// An execution identity is a fencing token, independent of durable peer identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, sqlx::Type)]
#[sqlx(type_name = "attempt_state")]
pub enum AttemptState {
    Claimed,
    Running,
    Completed,
    Failed,
    Expired,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct ExecutionAttempt {
    pub id: AttemptId,
    pub issue_id: IssueId,
    pub peer_id: PeerId,
    pub state: AttemptState,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub heartbeat_at: DateTime<Utc>,
    pub lease_expires_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    pub issue: Issue,
    pub attempt: ExecutionAttempt,
}

#[cfg(test)]
mod tests {
    use super::{IssueId, IssueStatus};

    #[test]
    fn terminal_states_are_final() {
        for status in [
            IssueStatus::Completed,
            IssueStatus::Failed,
            IssueStatus::Cancelled,
        ] {
            assert!(status.is_terminal());
        }
        for status in [
            IssueStatus::Backlog,
            IssueStatus::Ready,
            IssueStatus::Claimed,
            IssueStatus::Running,
            IssueStatus::Blocked,
            IssueStatus::WaitingForHuman,
        ] {
            assert!(!status.is_terminal());
        }
    }

    #[test]
    fn issue_ids_round_trip_and_reject_invalid_input() {
        let text = "c6b822ad-b0c5-457e-964c-79165e7ca746";
        assert_eq!(text.parse::<IssueId>().unwrap().to_string(), text);
        assert_eq!(
            "not-an-id".parse::<IssueId>().unwrap_err().to_string(),
            "invalid character: found `n` at 0"
        );
    }
}
