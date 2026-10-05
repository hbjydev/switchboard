//! Version-one wire types. Database row structs are never the HTTP contract.
use chrono::{DateTime, Utc};
use ledger::{AttemptState, IssueKind, IssueStatus, NewIssue};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize)]
pub enum IssueKindDto {
    #[default]
    Task,
    Question,
    Approval,
}

impl From<IssueKindDto> for IssueKind {
    fn from(value: IssueKindDto) -> Self {
        match value {
            IssueKindDto::Task => Self::Task,
            IssueKindDto::Question => Self::Question,
            IssueKindDto::Approval => Self::Approval,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CreateIssueRequest {
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub kind: IssueKindDto,
    #[serde(default)]
    pub priority: i32,
    #[serde(default)]
    pub backlog: bool,
}

impl CreateIssueRequest {
    pub(super) fn into_issue(self) -> NewIssue {
        NewIssue {
            title: self.title,
            description: self.description,
            kind: self.kind.into(),
            priority: self.priority,
            backlog: self.backlog,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EmptyRequest {}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AnswerRequest {
    pub answer: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CancelRequest {
    pub reason: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DependencyRequest {
    pub dependency_id: Uuid,
}

#[derive(Deserialize, Serialize)]
pub struct IssueResponse {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub kind: String,
    pub status: String,
    pub created_by: Uuid,
    pub owner: Option<Uuid>,
    pub current_attempt_id: Option<Uuid>,
    pub parent_id: Option<Uuid>,
    pub priority: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<ledger::Issue> for IssueResponse {
    fn from(issue: ledger::Issue) -> Self {
        Self {
            id: issue.id.0,
            title: issue.title,
            description: issue.description,
            kind: match issue.kind {
                IssueKind::Task => "Task",
                IssueKind::Question => "Question",
                IssueKind::Approval => "Approval",
            }
            .to_owned(),
            status: match issue.status {
                IssueStatus::Backlog => "Backlog",
                IssueStatus::Ready => "Ready",
                IssueStatus::Claimed => "Claimed",
                IssueStatus::Running => "Running",
                IssueStatus::Blocked => "Blocked",
                IssueStatus::WaitingForHuman => "WaitingForHuman",
                IssueStatus::Completed => "Completed",
                IssueStatus::Failed => "Failed",
                IssueStatus::Cancelled => "Cancelled",
            }
            .to_owned(),
            created_by: issue.created_by.0,
            owner: issue.owner.map(|id| id.0),
            current_attempt_id: issue.current_attempt_id.map(|id| id.0),
            parent_id: issue.parent_id.map(|id| id.0),
            priority: issue.priority,
            created_at: issue.created_at,
            updated_at: issue.updated_at,
        }
    }
}

#[derive(Deserialize, Serialize)]
pub struct EventResponse {
    pub id: i64,
    pub issue_id: Uuid,
    pub event_type: String,
    pub actor: Option<Uuid>,
    pub occurred_at: DateTime<Utc>,
    pub metadata: serde_json::Value,
}

impl From<ledger::IssueEvent> for EventResponse {
    fn from(event: ledger::IssueEvent) -> Self {
        Self {
            id: event.id,
            issue_id: event.issue_id.0,
            event_type: event.event_type,
            actor: event.actor.map(|id| id.0),
            occurred_at: event.occurred_at,
            metadata: event.metadata,
        }
    }
}

#[derive(Deserialize, Serialize)]
pub struct PeerResponse {
    pub id: Uuid,
    pub name: String,
    pub kind: String,
}

#[derive(Deserialize, Serialize)]
pub struct AttemptResponse {
    pub id: Uuid,
    pub issue_id: Uuid,
    pub peer_id: Uuid,
    pub state: String,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub heartbeat_at: DateTime<Utc>,
    pub lease_expires_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Deserialize, Serialize)]
pub struct AttemptRecordResponse {
    pub current: bool,
    pub peer: Option<PeerResponse>,
    pub attempt: AttemptResponse,
}

impl From<crate::AttemptRecord> for AttemptRecordResponse {
    fn from(record: crate::AttemptRecord) -> Self {
        let attempt = record.attempt;
        Self {
            current: record.current,
            peer: record.peer.map(|peer| PeerResponse {
                id: peer.id.0,
                name: peer.name,
                kind: match peer.kind {
                    ledger::PeerKind::Human => "Human",
                    ledger::PeerKind::Agent => "Agent",
                }
                .to_owned(),
            }),
            attempt: AttemptResponse {
                id: attempt.id.0,
                issue_id: attempt.issue_id.0,
                peer_id: attempt.peer_id.0,
                state: match attempt.state {
                    AttemptState::Claimed => "Claimed",
                    AttemptState::Running => "Running",
                    AttemptState::Completed => "Completed",
                    AttemptState::Failed => "Failed",
                    AttemptState::Expired => "Expired",
                    AttemptState::Cancelled => "Cancelled",
                }
                .to_owned(),
                created_at: attempt.created_at,
                started_at: attempt.started_at,
                heartbeat_at: attempt.heartbeat_at,
                lease_expires_at: attempt.lease_expires_at,
                finished_at: attempt.finished_at,
            },
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct StatusResponse {
    pub version: String,
    pub scheduler_enabled: bool,
    pub scheduler_concurrency: usize,
    pub backend_kind: String,
}

#[derive(Deserialize, Serialize)]
pub struct IdentityResponse {
    pub issuer: String,
    pub subject: String,
    pub kind: String,
    pub client_id: Option<String>,
    pub peer_id: Uuid,
    pub scopes: Vec<String>,
}

#[derive(Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: ErrorDetail,
}

#[derive(Serialize, Deserialize)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
}
