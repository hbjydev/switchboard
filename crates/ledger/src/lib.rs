//! Persistent work and controlled lifecycle operations, independent of executors.
mod execution;
mod model;
pub use model::*;
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use std::time::Duration;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database: {0}")]
    Database(#[from] sqlx::Error),
    #[error("migration: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),
    #[error("issue {0} not found")]
    NotFound(IssueId),
    #[error("invalid transition from {0:?}")]
    InvalidTransition(IssueStatus),
    #[error("actor is not eligible or does not own the claim")]
    IneligibleActor,
    #[error("dependency would create a cycle")]
    DependencyCycle,
    #[error("{0} must not be blank")]
    EmptyField(&'static str),
    #[error("peer name already belongs to a different kind")]
    PeerKindConflict,
    #[error("execution attempt {0} has lost its lease or authority")]
    ExecutionLost(AttemptId),
    #[error("lease duration must be between 3 milliseconds and 1 day")]
    InvalidLeaseDuration,
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone)]
pub struct Ledger {
    pool: PgPool,
    lease_duration: Duration,
}
impl Ledger {
    #[must_use]
    pub const fn from_pool(pool: PgPool) -> Self {
        Self {
            pool,
            lease_duration: Duration::from_secs(30),
        }
    }
    pub async fn connect(url: &str) -> Result<Self> {
        Ok(Self::from_pool(PgPool::connect(url).await?))
    }
    pub async fn migrate(&self) -> Result<()> {
        sqlx::migrate!("../../migrations").run(&self.pool).await?;
        Ok(())
    }
    // Graph changes and lifecycle changes share one lock. Claims take its shared
    // form, then row locks: concurrent claimers still use SKIP LOCKED independently.
    async fn transaction(&self, shared: bool) -> Result<Transaction<'_, Postgres>> {
        let mut tx = self.pool.begin().await?;
        let query = if shared {
            "SELECT pg_advisory_xact_lock_shared(741932)"
        } else {
            "SELECT pg_advisory_xact_lock(741932)"
        };
        sqlx::query(query).execute(&mut *tx).await?;
        Ok(tx)
    }
    pub async fn ensure_peer(&self, name: &str, kind: PeerKind) -> Result<Peer> {
        nonempty(name, "peer name")?;
        let peer: Peer = sqlx::query_as("INSERT INTO peers(id,name,kind) VALUES ($1,$2,$3) ON CONFLICT(name) DO UPDATE SET name=EXCLUDED.name RETURNING *")
            .bind(PeerId(Uuid::new_v4())).bind(name).bind(kind).fetch_one(&self.pool).await?;
        if peer.kind != kind {
            return Err(Error::PeerKindConflict);
        }
        Ok(peer)
    }
    pub async fn peer(&self, id: PeerId) -> Result<Option<Peer>> {
        Ok(sqlx::query_as("SELECT * FROM peers WHERE id=$1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?)
    }
    /// Human work intake. Execution-generated work uses `create_child_for_attempt`.
    pub async fn create_issue(&self, actor: PeerId, new: NewIssue) -> Result<Issue> {
        let mut tx = self.transaction(false).await?;
        require_kind(&mut tx, actor, PeerKind::Human).await?;
        let issue = insert_issue(&mut tx, actor, new, None).await?;
        tx.commit().await?;
        Ok(issue)
    }
    pub async fn get_issue(&self, id: IssueId) -> Result<Issue> {
        sqlx::query_as("SELECT * FROM issues WHERE id=$1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(Error::NotFound(id))
    }
    pub async fn list_issues(&self) -> Result<Vec<Issue>> {
        Ok(
            sqlx::query_as("SELECT * FROM issues ORDER BY created_at,id")
                .fetch_all(&self.pool)
                .await?,
        )
    }
    pub async fn list_ready_issues(&self) -> Result<Vec<Issue>> {
        Ok(sqlx::query_as(
            "SELECT * FROM issues WHERE status='Ready' ORDER BY priority DESC,created_at,id",
        )
        .fetch_all(&self.pool)
        .await?)
    }
    pub async fn children(&self, id: IssueId) -> Result<Vec<Issue>> {
        self.get_issue(id).await?;
        Ok(
            sqlx::query_as("SELECT * FROM issues WHERE parent_id=$1 ORDER BY created_at,id")
                .bind(id)
                .fetch_all(&self.pool)
                .await?,
        )
    }
    /// Direct prerequisites, including completed prerequisites.
    pub async fn dependencies(&self, id: IssueId) -> Result<Vec<Issue>> {
        self.get_issue(id).await?;
        Ok(sqlx::query_as("SELECT i.* FROM issues i JOIN issue_dependencies d ON d.dependency_id=i.id WHERE d.issue_id=$1 ORDER BY i.id")
            .bind(id).fetch_all(&self.pool).await?)
    }

    pub async fn events(&self, id: IssueId) -> Result<Vec<IssueEvent>> {
        self.get_issue(id).await?;
        Ok(
            sqlx::query_as("SELECT * FROM issue_events WHERE issue_id=$1 ORDER BY id")
                .bind(id)
                .fetch_all(&self.pool)
                .await?,
        )
    }
    pub async fn make_ready(&self, id: IssueId, actor: PeerId) -> Result<Issue> {
        let mut tx = self.transaction(false).await?;
        require_kind(&mut tx, actor, PeerKind::Human).await?;
        let issue = locked_issue(&mut tx, id).await?;
        require_status(&issue, &[IssueStatus::Backlog])?;
        let result = set_state(
            &mut tx,
            &issue,
            IssueStatus::Ready,
            None,
            actor,
            "IssueReadied",
            json!({}),
        )
        .await?;
        refresh_blocking(&mut tx, id, actor).await?;
        let result = locked_issue(&mut tx, result.id).await?;
        tx.commit().await?;
        Ok(result)
    }
    /// Human completion of an unclaimed Ready task. Workers use `complete_attempt`.
    pub async fn complete_issue(&self, id: IssueId, actor: PeerId) -> Result<Issue> {
        let mut tx = self.transaction(false).await?;
        let issue = locked_issue(&mut tx, id).await?;
        require_status(&issue, &[IssueStatus::Ready])?;
        require_kind(&mut tx, actor, PeerKind::Human).await?;
        let result = finish(
            &mut tx,
            &issue,
            actor,
            IssueStatus::Completed,
            "IssueCompleted",
            json!({}),
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn cancel_issue(&self, id: IssueId, actor: PeerId, reason: &str) -> Result<Issue> {
        nonempty(reason, "cancellation reason")?;
        let mut tx = self.transaction(false).await?;
        let issue = locked_issue(&mut tx, id).await?;
        if issue.status.is_terminal() {
            return Err(Error::InvalidTransition(issue.status));
        }
        require_kind(&mut tx, actor, PeerKind::Human).await?;
        let result = finish(
            &mut tx,
            &issue,
            actor,
            IssueStatus::Cancelled,
            "IssueCancelled",
            json!({"reason":reason}),
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn resolve_human_issue(
        &self,
        id: IssueId,
        human: PeerId,
        answer: &str,
    ) -> Result<Issue> {
        nonempty(answer, "answer")?;
        let mut tx = self.transaction(false).await?;
        require_kind(&mut tx, human, PeerKind::Human).await?;
        let issue = locked_issue(&mut tx, id).await?;
        if issue.kind == IssueKind::Task {
            return Err(Error::InvalidTransition(issue.status));
        }
        require_status(&issue, &[IssueStatus::WaitingForHuman])?;
        let result = finish(
            &mut tx,
            &issue,
            human,
            IssueStatus::Completed,
            "HumanIssueResolved",
            json!({"answer":answer}),
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn add_dependency(
        &self,
        id: IssueId,
        dependency: IssueId,
        actor: PeerId,
    ) -> Result<()> {
        let mut tx = self.transaction(false).await?;
        require_kind(&mut tx, actor, PeerKind::Human).await?;
        add_dependency(&mut tx, id, dependency, actor).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn create_child_issue(
        &self,
        parent: IssueId,
        actor: PeerId,
        new: NewIssue,
        blocking: bool,
    ) -> Result<Issue> {
        let mut tx = self.transaction(false).await?;
        require_kind(&mut tx, actor, PeerKind::Human).await?;
        let issue = locked_issue(&mut tx, parent).await?;
        editable(&issue)?;
        let child = insert_issue(&mut tx, actor, new, Some(parent)).await?;
        event(
            &mut tx,
            parent,
            actor,
            "ChildIssueCreated",
            json!({"child_id":child.id,"blocking":blocking}),
        )
        .await?;
        if blocking {
            add_dependency(&mut tx, parent, child.id, actor).await?;
        }
        tx.commit().await?;
        Ok(child)
    }
}
fn nonempty(value: &str, field: &'static str) -> Result<()> {
    if value.trim().is_empty() {
        Err(Error::EmptyField(field))
    } else {
        Ok(())
    }
}
fn require_status(issue: &Issue, allowed: &[IssueStatus]) -> Result<()> {
    if allowed.contains(&issue.status) {
        Ok(())
    } else {
        Err(Error::InvalidTransition(issue.status))
    }
}
fn editable(issue: &Issue) -> Result<()> {
    if issue.status.is_terminal() || issue.kind != IssueKind::Task {
        return Err(Error::InvalidTransition(issue.status));
    }
    Ok(())
}
async fn require_kind(db: &mut PgConnection, id: PeerId, kind: PeerKind) -> Result<()> {
    let actual: Option<PeerKind> = sqlx::query_scalar("SELECT kind FROM peers WHERE id=$1")
        .bind(id)
        .fetch_optional(db)
        .await?;
    if actual == Some(kind) {
        Ok(())
    } else {
        Err(Error::IneligibleActor)
    }
}
async fn locked_issue(db: &mut PgConnection, id: IssueId) -> Result<Issue> {
    sqlx::query_as("SELECT * FROM issues WHERE id=$1 FOR UPDATE")
        .bind(id)
        .fetch_optional(db)
        .await?
        .ok_or(Error::NotFound(id))
}
async fn event(
    db: &mut PgConnection,
    id: IssueId,
    actor: PeerId,
    kind: &str,
    metadata: Value,
) -> Result<()> {
    sqlx::query("INSERT INTO issue_events(issue_id,actor,event_type,metadata) VALUES($1,$2,$3,$4)")
        .bind(id)
        .bind(actor)
        .bind(kind)
        .bind(metadata)
        .execute(db)
        .await?;
    Ok(())
}
async fn insert_issue(
    db: &mut PgConnection,
    actor: PeerId,
    new: NewIssue,
    parent: Option<IssueId>,
) -> Result<Issue> {
    nonempty(&new.title, "title")?;
    let status = if new.kind != IssueKind::Task {
        IssueStatus::WaitingForHuman
    } else if new.backlog {
        IssueStatus::Backlog
    } else {
        IssueStatus::Ready
    };
    let issue:Issue=sqlx::query_as("INSERT INTO issues(id,title,description,kind,status,created_by,parent_id,priority) VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING *")
        .bind(IssueId(Uuid::new_v4())).bind(new.title).bind(new.description).bind(new.kind).bind(status).bind(actor).bind(parent).bind(new.priority).fetch_one(&mut *db).await?;
    event(
        db,
        issue.id,
        actor,
        "IssueCreated",
        json!({"kind":issue.kind,"status":status,"parent_id":parent}),
    )
    .await?;
    Ok(issue)
}
async fn set_state(
    db: &mut PgConnection,
    issue: &Issue,
    status: IssueStatus,
    owner: Option<PeerId>,
    actor: PeerId,
    kind: &str,
    mut metadata: Value,
) -> Result<Issue> {
    let active = matches!(status, IssueStatus::Claimed | IssueStatus::Running);
    if !active {
        let state = match status {
            IssueStatus::Completed => AttemptState::Completed,
            IssueStatus::Failed => AttemptState::Failed,
            _ => AttemptState::Cancelled,
        };
        execution::end_attempt(db, issue, state, actor).await?;
    }
    if let Some(fields) = metadata.as_object_mut() {
        if let Some(attempt_id) = issue.current_attempt_id {
            fields.insert("attempt_id".to_owned(), json!(attempt_id));
        }
        fields.insert("from".to_owned(), json!(issue.status));
        fields.insert("to".to_owned(), json!(status));
    }
    let updated = sqlx::query_as(
        "UPDATE issues SET status=$2,owner=$3,current_attempt_id=$4,updated_at=clock_timestamp() WHERE id=$1 RETURNING *",
    )
    .bind(issue.id)
    .bind(status)
    .bind(owner)
    .bind(if active { issue.current_attempt_id } else { None })
    .fetch_one(&mut *db)
    .await?;
    event(db, issue.id, actor, kind, metadata).await?;
    Ok(updated)
}
async fn finish(
    db: &mut PgConnection,
    issue: &Issue,
    actor: PeerId,
    status: IssueStatus,
    kind: &str,
    metadata: Value,
) -> Result<Issue> {
    let updated = set_state(db, issue, status, issue.owner, actor, kind, metadata).await?;
    let ids: Vec<IssueId> = sqlx::query_scalar(
        "SELECT issue_id FROM issue_dependencies WHERE dependency_id=$1 ORDER BY issue_id",
    )
    .bind(issue.id)
    .fetch_all(&mut *db)
    .await?;
    for id in ids {
        refresh_blocking(db, id, actor).await?;
    }
    Ok(updated)
}
async fn add_dependency(
    db: &mut PgConnection,
    id: IssueId,
    dependency: IssueId,
    actor: PeerId,
) -> Result<()> {
    let issue = locked_issue(db, id).await?;
    editable(&issue)?;
    locked_issue(db, dependency).await?;
    let cycle:bool=sqlx::query_scalar("WITH RECURSIVE reachable(id) AS (SELECT $1::uuid UNION SELECT d.dependency_id FROM issue_dependencies d JOIN reachable r ON d.issue_id=r.id) SELECT EXISTS(SELECT 1 FROM reachable WHERE id=$2)")
        .bind(dependency).bind(id).fetch_one(&mut *db).await?;
    if cycle {
        return Err(Error::DependencyCycle);
    }
    let result=sqlx::query("INSERT INTO issue_dependencies(issue_id,dependency_id) VALUES($1,$2) ON CONFLICT DO NOTHING").bind(id).bind(dependency).execute(&mut *db).await?;
    if result.rows_affected() > 0 {
        event(
            db,
            id,
            actor,
            "DependencyAdded",
            json!({"dependency_id":dependency}),
        )
        .await?;
    }
    refresh_blocking(db, id, actor).await
}
async fn refresh_blocking(db: &mut PgConnection, id: IssueId, actor: PeerId) -> Result<()> {
    let issue = locked_issue(db, id).await?;
    if issue.status.is_terminal() || issue.status == IssueStatus::Backlog {
        return Ok(());
    }
    let unresolved:Vec<Issue>=sqlx::query_as("SELECT i.* FROM issues i JOIN issue_dependencies d ON d.dependency_id=i.id WHERE d.issue_id=$1 AND i.status <> 'Completed' ORDER BY i.id")
        .bind(id).fetch_all(&mut *db).await?;
    let status = if unresolved.is_empty() {
        if !matches!(
            issue.status,
            IssueStatus::Blocked | IssueStatus::WaitingForHuman
        ) {
            return Ok(());
        }
        IssueStatus::Ready
    } else if unresolved
        .iter()
        .any(|i| i.kind != IssueKind::Task && i.status == IssueStatus::WaitingForHuman)
    {
        IssueStatus::WaitingForHuman
    } else {
        IssueStatus::Blocked
    };
    if status != issue.status {
        let kind = if status == IssueStatus::Ready {
            "IssueUnblocked"
        } else {
            "IssueBlocked"
        };
        set_state(
            db,
            &issue,
            status,
            None,
            actor,
            kind,
            json!({"unresolved_dependencies":unresolved.iter().map(|i|i.id).collect::<Vec<_>>()}),
        )
        .await?;
    }
    Ok(())
}
