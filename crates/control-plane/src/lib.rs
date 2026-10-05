//! Shared application operations and lifecycle for the persistent control plane.
pub mod auth;
pub mod http;
pub mod scheduler;

use ledger::{
    ExecutionAttempt, Issue, IssueEvent, IssueId, Ledger, NewIssue, Peer, PeerId, PeerKind,
};

/// Application operations shared by the HTTP boundary and legacy direct CLI.
#[derive(Clone)]
pub struct Application {
    ledger: Ledger,
}

/// Attempt history enriched with durable peer identity and current ownership.
pub struct AttemptRecord {
    pub current: bool,
    pub peer: Option<Peer>,
    pub attempt: ExecutionAttempt,
}

impl Application {
    #[must_use]
    pub const fn new(ledger: Ledger) -> Self {
        Self { ledger }
    }

    pub async fn check_ready(&self) -> ledger::Result<()> {
        self.ledger.check_ready().await
    }

    pub async fn create(&self, actor: &str, issue: NewIssue) -> ledger::Result<Issue> {
        let human = self.ledger.ensure_peer(actor, PeerKind::Human).await?;
        self.create_as(human.id, issue).await
    }

    pub async fn identity(
        &self,
        issuer: &str,
        subject: &str,
        kind: PeerKind,
    ) -> ledger::Result<Peer> {
        self.ledger
            .ensure_authenticated_peer(issuer, subject, kind)
            .await
    }

    pub async fn create_as(&self, actor: PeerId, issue: NewIssue) -> ledger::Result<Issue> {
        self.ledger.create_issue(actor, issue).await
    }

    pub async fn list(&self, ready: bool) -> ledger::Result<Vec<Issue>> {
        if ready {
            self.ledger.list_ready_issues().await
        } else {
            self.ledger.list_issues().await
        }
    }

    pub async fn get(&self, id: IssueId) -> ledger::Result<Issue> {
        self.ledger.get_issue(id).await
    }

    pub async fn events(&self, id: IssueId) -> ledger::Result<Vec<IssueEvent>> {
        self.ledger.events(id).await
    }

    pub async fn attempts(&self, id: IssueId) -> ledger::Result<Vec<AttemptRecord>> {
        let issue = self.get(id).await?;
        let mut records = Vec::new();
        for attempt in self.ledger.attempts(id).await? {
            records.push(AttemptRecord {
                current: issue.current_attempt_id == Some(attempt.id),
                peer: self.ledger.peer(attempt.peer_id).await?,
                attempt,
            });
        }
        Ok(records)
    }

    pub async fn ready(&self, actor: &str, id: IssueId) -> ledger::Result<Issue> {
        let human = self.ledger.ensure_peer(actor, PeerKind::Human).await?;
        self.ready_as(human.id, id).await
    }

    pub async fn complete(&self, actor: &str, id: IssueId) -> ledger::Result<Issue> {
        let human = self.ledger.ensure_peer(actor, PeerKind::Human).await?;
        self.complete_as(human.id, id).await
    }

    pub async fn cancel(&self, actor: &str, id: IssueId, reason: &str) -> ledger::Result<Issue> {
        let human = self.ledger.ensure_peer(actor, PeerKind::Human).await?;
        self.cancel_as(human.id, id, reason).await
    }

    pub async fn answer(&self, actor: &str, id: IssueId, answer: &str) -> ledger::Result<Issue> {
        let human = self.ledger.ensure_peer(actor, PeerKind::Human).await?;
        self.answer_as(human.id, id, answer).await
    }

    pub async fn depend(
        &self,
        actor: &str,
        id: IssueId,
        dependency: IssueId,
    ) -> ledger::Result<()> {
        let human = self.ledger.ensure_peer(actor, PeerKind::Human).await?;
        self.depend_as(human.id, id, dependency).await
    }

    pub async fn ready_as(&self, actor: PeerId, id: IssueId) -> ledger::Result<Issue> {
        self.ledger.make_ready(id, actor).await
    }

    pub async fn complete_as(&self, actor: PeerId, id: IssueId) -> ledger::Result<Issue> {
        self.ledger.complete_issue(id, actor).await
    }

    pub async fn cancel_as(
        &self,
        actor: PeerId,
        id: IssueId,
        reason: &str,
    ) -> ledger::Result<Issue> {
        self.ledger.cancel_issue(id, actor, reason).await
    }

    pub async fn answer_as(
        &self,
        actor: PeerId,
        id: IssueId,
        answer: &str,
    ) -> ledger::Result<Issue> {
        self.ledger.resolve_human_issue(id, actor, answer).await
    }

    pub async fn depend_as(
        &self,
        actor: PeerId,
        id: IssueId,
        dependency: IssueId,
    ) -> ledger::Result<()> {
        self.ledger.add_dependency(id, dependency, actor).await
    }
}
