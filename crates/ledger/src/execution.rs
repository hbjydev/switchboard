use super::{
    AttemptId, AttemptState, Claim, Error, ExecutionAttempt, Issue, IssueId, IssueStatus, Ledger,
    NewIssue, PeerId, PeerKind, Result, add_dependency, event, finish, insert_issue, locked_issue,
    nonempty, refresh_blocking, require_kind, require_status, set_state,
};
use serde_json::{Value, json};
use sqlx::PgConnection;
use std::time::Duration;
use uuid::Uuid;

impl Ledger {
    /// The same lease duration applies to claims and renewals made by this handle.
    pub fn with_lease_duration(mut self, duration: Duration) -> Result<Self> {
        if duration < Duration::from_millis(3) || duration > Duration::from_hours(24) {
            return Err(Error::InvalidLeaseDuration);
        }
        self.lease_duration = duration;
        Ok(self)
    }

    #[must_use]
    pub const fn lease_duration(&self) -> Duration {
        self.lease_duration
    }

    fn lease_micros(&self) -> Result<i64> {
        i64::try_from(self.lease_duration.as_micros()).map_err(|_error| Error::InvalidLeaseDuration)
    }

    pub async fn attempts(&self, id: IssueId) -> Result<Vec<ExecutionAttempt>> {
        self.get_issue(id).await?;
        Ok(sqlx::query_as(
            "SELECT * FROM execution_attempts WHERE issue_id=$1 ORDER BY created_at,id",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await?)
    }

    pub async fn claim_next_issue(&self, actor: PeerId) -> Result<Option<Claim>> {
        self.claim(None, actor).await
    }

    pub async fn claim_issue(&self, id: IssueId, actor: PeerId) -> Result<Claim> {
        match self.claim(Some(id), actor).await? {
            Some(claim) => Ok(claim),
            None => Err(Error::InvalidTransition(self.get_issue(id).await?.status)),
        }
    }

    async fn claim(&self, id: Option<IssueId>, actor: PeerId) -> Result<Option<Claim>> {
        let mut tx = self.transaction(true).await?;
        require_kind(&mut tx, actor, PeerKind::Agent).await?;
        let issue: Option<Issue> = sqlx::query_as("SELECT * FROM issues WHERE status='Ready' AND kind='Task' AND ($1::uuid IS NULL OR id=$1) ORDER BY priority DESC,created_at,id FOR UPDATE SKIP LOCKED LIMIT 1")
            .bind(id).fetch_optional(&mut *tx).await?;
        let result = if let Some(mut issue) = issue {
            let attempt: ExecutionAttempt = sqlx::query_as("INSERT INTO execution_attempts(id,issue_id,peer_id,state,lease_expires_at) VALUES($1,$2,$3,'Claimed',clock_timestamp()+$4::bigint*interval '1 microsecond') RETURNING *")
                .bind(AttemptId(Uuid::new_v4())).bind(issue.id).bind(actor).bind(self.lease_micros()?).fetch_one(&mut *tx).await?;
            issue.current_attempt_id = Some(attempt.id);
            let issue = set_state(
                &mut tx,
                &issue,
                IssueStatus::Claimed,
                Some(actor),
                actor,
                "IssueClaimed",
                json!({}),
            )
            .await?;
            event(
                &mut tx,
                issue.id,
                actor,
                "ExecutionClaimed",
                json!({"attempt_id":attempt.id,"lease_expires_at":attempt.lease_expires_at}),
            )
            .await?;
            Some(Claim { issue, attempt })
        } else {
            None
        };
        tx.commit().await?;
        Ok(result)
    }

    pub async fn mark_running(&self, id: AttemptId) -> Result<Issue> {
        let mut tx = self.transaction(false).await?;
        let (issue, attempt) = active_attempt(&mut tx, id).await?;
        require_status(&issue, &[IssueStatus::Claimed])?;
        sqlx::query("UPDATE execution_attempts SET state='Running',started_at=clock_timestamp() WHERE id=$1")
            .bind(id).execute(&mut *tx).await?;
        let result = set_state(
            &mut tx,
            &issue,
            IssueStatus::Running,
            Some(attempt.peer_id),
            attempt.peer_id,
            "IssueStarted",
            json!({}),
        )
        .await?;
        event(
            &mut tx,
            issue.id,
            attempt.peer_id,
            "ExecutionStarted",
            json!({"attempt_id":id}),
        )
        .await?;
        tx.commit().await?;
        Ok(result)
    }

    /// Heartbeats update attempt metadata only, keeping Issue event history quiet.
    pub async fn heartbeat(&self, id: AttemptId) -> Result<ExecutionAttempt> {
        let mut tx = self.transaction(true).await?;
        active_attempt(&mut tx, id).await?;
        let attempt = sqlx::query_as("UPDATE execution_attempts SET heartbeat_at=clock_timestamp(),lease_expires_at=clock_timestamp()+$2::bigint*interval '1 microsecond' WHERE id=$1 RETURNING *")
            .bind(id).bind(self.lease_micros()?).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(attempt)
    }

    pub async fn complete_attempt(&self, id: AttemptId) -> Result<Issue> {
        self.finish_execution(id, IssueStatus::Completed, "IssueCompleted", json!({}))
            .await
    }

    pub async fn fail_attempt(&self, id: AttemptId, reason: &str) -> Result<Issue> {
        nonempty(reason, "failure reason")?;
        self.finish_execution(
            id,
            IssueStatus::Failed,
            "IssueFailed",
            json!({"reason":reason}),
        )
        .await
    }

    async fn finish_execution(
        &self,
        id: AttemptId,
        status: IssueStatus,
        kind: &str,
        metadata: Value,
    ) -> Result<Issue> {
        let mut tx = self.transaction(false).await?;
        let (issue, attempt) = active_attempt(&mut tx, id).await?;
        if status == IssueStatus::Completed {
            require_status(&issue, &[IssueStatus::Running])?;
        }
        let result = finish(&mut tx, &issue, attempt.peer_id, status, kind, metadata).await?;
        tx.commit().await?;
        Ok(result)
    }

    /// Apply child/human handoff as one terminal outcome. Replays conflict before
    /// inserting another child. Nonblocking children finish the parent's work.
    pub async fn create_child_for_attempt(
        &self,
        id: AttemptId,
        new: NewIssue,
        blocking: bool,
    ) -> Result<Issue> {
        let mut tx = self.transaction(false).await?;
        let (issue, attempt) = active_attempt(&mut tx, id).await?;
        require_status(&issue, &[IssueStatus::Running])?;
        let actor = attempt.peer_id;
        let child = insert_issue(&mut tx, actor, new, Some(issue.id)).await?;
        event(
            &mut tx,
            issue.id,
            actor,
            "ChildIssueCreated",
            json!({"attempt_id":id,"child_id":child.id,"blocking":blocking}),
        )
        .await?;
        if blocking {
            end_attempt(&mut tx, &issue, AttemptState::Completed, actor).await?;
            add_dependency(&mut tx, issue.id, child.id, actor).await?;
        } else {
            finish(
                &mut tx,
                &issue,
                actor,
                IssueStatus::Completed,
                "IssueCompleted",
                json!({"child_id":child.id}),
            )
            .await?;
        }
        tx.commit().await?;
        Ok(child)
    }

    /// Maintenance is explicit. Workers call this before claiming, never upgrade
    /// a shared claim lock, and recovery and graph changes serialize together.
    pub async fn recover_expired_attempts(&self) -> Result<Vec<IssueId>> {
        let mut tx = self.transaction(false).await?;
        let issues: Vec<Issue> = sqlx::query_as("SELECT i.* FROM issues i JOIN execution_attempts a ON a.id=i.current_attempt_id WHERE i.status IN ('Claimed','Running') AND a.state IN ('Claimed','Running') AND a.lease_expires_at<=clock_timestamp() ORDER BY i.id FOR UPDATE OF i SKIP LOCKED")
            .fetch_all(&mut *tx).await?;
        let mut recovered = Vec::new();
        for issue in issues {
            let actor = issue.owner.ok_or(Error::InvalidTransition(issue.status))?;
            end_attempt(&mut tx, &issue, AttemptState::Expired, actor).await?;
            set_state(
                &mut tx,
                &issue,
                IssueStatus::Ready,
                None,
                actor,
                "ExecutionRecovered",
                json!({"reason":"lease expired"}),
            )
            .await?;
            refresh_blocking(&mut tx, issue.id, actor).await?;
            recovered.push(issue.id);
        }
        tx.commit().await?;
        Ok(recovered)
    }
}

/// Lock ordering is always advisory lock -> issue -> attempt. Read the immutable
/// `issue_id` first; all authority and DB-clock lease checks happen after row locks.
async fn active_attempt(db: &mut PgConnection, id: AttemptId) -> Result<(Issue, ExecutionAttempt)> {
    let issue_id: IssueId =
        sqlx::query_scalar("SELECT issue_id FROM execution_attempts WHERE id=$1")
            .bind(id)
            .fetch_optional(&mut *db)
            .await?
            .ok_or(Error::ExecutionLost(id))?;
    let issue = locked_issue(db, issue_id).await?;
    let attempt: ExecutionAttempt =
        sqlx::query_as("SELECT * FROM execution_attempts WHERE id=$1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut *db)
            .await?;
    let valid: bool = sqlx::query_scalar(
        "SELECT lease_expires_at>clock_timestamp() FROM execution_attempts WHERE id=$1",
    )
    .bind(id)
    .fetch_one(&mut *db)
    .await?;
    if !valid
        || issue.current_attempt_id != Some(id)
        || issue.owner != Some(attempt.peer_id)
        || !matches!(issue.status, IssueStatus::Claimed | IssueStatus::Running)
        || !matches!(attempt.state, AttemptState::Claimed | AttemptState::Running)
    {
        return Err(Error::ExecutionLost(id));
    }
    Ok((issue, attempt))
}

pub async fn end_attempt(
    db: &mut PgConnection,
    issue: &Issue,
    state: AttemptState,
    actor: PeerId,
) -> Result<()> {
    let Some(id) = issue.current_attempt_id else {
        return Ok(());
    };
    let result = sqlx::query("UPDATE execution_attempts SET state=$2,finished_at=clock_timestamp() WHERE id=$1 AND state IN ('Claimed','Running')")
        .bind(id).bind(state).execute(&mut *db).await?;
    if result.rows_affected() > 0 {
        let kind = match state {
            AttemptState::Completed => "ExecutionCompleted",
            AttemptState::Failed => "ExecutionFailed",
            AttemptState::Expired => "ExecutionExpired",
            _ => "ExecutionCancelled",
        };
        event(db, issue.id, actor, kind, json!({"attempt_id":id})).await?;
    }
    Ok(())
}
