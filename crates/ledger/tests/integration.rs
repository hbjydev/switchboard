use ledger::{Error, IssueKind, IssueStatus, Ledger, NewIssue, PeerKind};
#[path = "../../../tests/support/mod.rs"]
mod support;
use support::TestDatabase;

#[tokio::test]
async fn authenticated_identity_binding_is_stable_atomic_and_kind_preserving() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let (first, second) = tokio::join!(
        ledger.ensure_authenticated_peer(
            "https://issuer.example.test",
            "immutable-subject",
            PeerKind::Human
        ),
        ledger.ensure_authenticated_peer(
            "https://issuer.example.test",
            "immutable-subject",
            PeerKind::Human
        )
    );
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(first.id, second.id);
    let restarted = Ledger::from_pool(database.pool.clone());
    assert_eq!(
        restarted
            .ensure_authenticated_peer(
                "https://issuer.example.test",
                "immutable-subject",
                PeerKind::Human
            )
            .await
            .unwrap()
            .id,
        first.id
    );
    let different_issuer = ledger
        .ensure_authenticated_peer(
            "https://other.example.test",
            "immutable-subject",
            PeerKind::Human,
        )
        .await
        .unwrap();
    assert_ne!(first.id, different_issuer.id);
    assert!(matches!(
        ledger
            .ensure_authenticated_peer(
                "https://issuer.example.test",
                "immutable-subject",
                PeerKind::Agent
            )
            .await,
        Err(Error::IdentityKindConflict)
    ));
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM authenticated_identities")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    let peers: i64 = sqlx::query_scalar("SELECT count(*) FROM peers")
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(count, 2);
    assert_eq!(peers, 2);
}

#[tokio::test]
async fn lifecycle_and_history() {
    let database = TestDatabase::start().await.unwrap();
    let pool = database.pool.clone();
    let ledger = Ledger::from_pool(pool.clone());
    let human = ledger.ensure_peer("human", PeerKind::Human).await.unwrap();
    let agent = ledger.ensure_peer("agent", PeerKind::Agent).await.unwrap();
    let issue = ledger
        .create_issue(human.id, NewIssue::task("work"))
        .await
        .unwrap();
    assert_eq!(ledger.list_ready_issues().await.unwrap().len(), 1);
    assert_eq!(ledger.get_issue(issue.id).await.unwrap().title, "work");
    assert!(matches!(
        ledger.claim_issue(issue.id, human.id).await,
        Err(Error::IneligibleActor)
    ));
    assert!(matches!(
        ledger.complete_issue(issue.id, agent.id).await,
        Err(Error::IneligibleActor)
    ));
    let claimed = ledger.claim_issue(issue.id, agent.id).await.unwrap();
    assert_eq!(claimed.issue.owner, Some(agent.id));
    assert_eq!(claimed.issue.current_attempt_id, Some(claimed.attempt.id));
    assert_eq!(claimed.issue.status, IssueStatus::Claimed);
    assert!(ledger.claim_next_issue(agent.id).await.unwrap().is_none());
    assert!(matches!(
        ledger.complete_issue(issue.id, agent.id).await,
        Err(Error::InvalidTransition(IssueStatus::Claimed))
    ));
    ledger.mark_running(claimed.attempt.id).await.unwrap();
    assert!(matches!(
        ledger.complete_issue(issue.id, human.id).await,
        Err(Error::InvalidTransition(IssueStatus::Running))
    ));
    ledger.complete_attempt(claimed.attempt.id).await.unwrap();
    assert_eq!(
        ledger.get_issue(issue.id).await.unwrap().status,
        IssueStatus::Completed
    );
    assert!(matches!(
        ledger.mark_running(claimed.attempt.id).await,
        Err(Error::ExecutionLost(id)) if id == claimed.attempt.id
    ));
    let events = ledger.events(issue.id).await.unwrap();
    let lifecycle = events
        .iter()
        .filter(|event| {
            matches!(
                event.event_type.as_str(),
                "IssueCreated" | "IssueClaimed" | "IssueStarted" | "IssueCompleted"
            )
        })
        .map(|event| event.event_type.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        lifecycle,
        vec![
            "IssueCreated",
            "IssueClaimed",
            "IssueStarted",
            "IssueCompleted"
        ]
    );
    assert_eq!(
        events
            .iter()
            .find(|event| event.event_type == "IssueCompleted")
            .unwrap()
            .metadata["from"],
        "Running"
    );
    assert!(matches!(
        sqlx::query("DELETE FROM issue_events WHERE issue_id=$1")
            .bind(issue.id)
            .execute(&pool)
            .await,
        Err(sqlx::Error::Database(_))
    ));
}

#[tokio::test]
async fn concurrent_claims_are_unique() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let human = ledger.ensure_peer("human", PeerKind::Human).await.unwrap();
    let mut agents = Vec::new();
    for i in 0..16 {
        agents.push(
            ledger
                .ensure_peer(&format!("agent-{i}"), PeerKind::Agent)
                .await
                .unwrap(),
        );
        ledger
            .create_issue(human.id, NewIssue::task(format!("work-{i}")))
            .await
            .unwrap();
    }
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(16));
    let mut tasks = tokio::task::JoinSet::new();
    for agent in agents {
        let ledger = ledger.clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            barrier.wait().await;
            ledger
                .claim_next_issue(agent.id)
                .await
                .unwrap()
                .unwrap()
                .issue
                .id
        });
    }
    let mut ids = std::collections::HashSet::new();
    while let Some(id) = tasks.join_next().await {
        assert!(ids.insert(id.unwrap()));
    }
    assert_eq!(ids.len(), 16);
    assert!(ledger.list_ready_issues().await.unwrap().is_empty());
}

#[tokio::test]
async fn dependencies_children_and_humans() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let human = ledger.ensure_peer("human", PeerKind::Human).await.unwrap();
    let agent = ledger.ensure_peer("agent", PeerKind::Agent).await.unwrap();
    let parent = ledger
        .create_issue(human.id, NewIssue::task("parent"))
        .await
        .unwrap();
    let free = ledger
        .create_child_issue(parent.id, human.id, NewIssue::task("independent"), false)
        .await
        .unwrap();
    assert_eq!(free.parent_id, Some(parent.id));
    assert_eq!(
        ledger.get_issue(parent.id).await.unwrap().status,
        IssueStatus::Ready
    );
    let child = ledger
        .create_child_issue(parent.id, human.id, NewIssue::task("blocking"), true)
        .await
        .unwrap();
    assert_eq!(
        ledger.get_issue(parent.id).await.unwrap().status,
        IssueStatus::Blocked
    );
    assert!(matches!(
        ledger.add_dependency(child.id, parent.id, human.id).await,
        Err(Error::DependencyCycle)
    ));
    assert!(matches!(
        ledger.add_dependency(parent.id, parent.id, human.id).await,
        Err(Error::DependencyCycle)
    ));
    let mut question = NewIssue::task("which option?");
    question.kind = IssueKind::Question;
    let question = ledger
        .create_child_issue(parent.id, human.id, question, true)
        .await
        .unwrap();
    assert_eq!(
        ledger.get_issue(parent.id).await.unwrap().status,
        IssueStatus::WaitingForHuman
    );
    assert!(matches!(
        ledger.resolve_human_issue(question.id, agent.id, "A").await,
        Err(Error::IneligibleActor)
    ));
    assert!(matches!(
        ledger.resolve_human_issue(question.id, human.id, "").await,
        Err(Error::EmptyField("answer"))
    ));
    ledger
        .resolve_human_issue(question.id, human.id, "A")
        .await
        .unwrap();
    assert_eq!(
        ledger.get_issue(parent.id).await.unwrap().status,
        IssueStatus::Blocked
    );
    ledger.complete_issue(child.id, human.id).await.unwrap();
    assert_eq!(
        ledger.get_issue(parent.id).await.unwrap().status,
        IssueStatus::Ready
    );
    assert_eq!(
        ledger.get_issue(free.id).await.unwrap().status,
        IssueStatus::Ready
    );
    assert!(
        ledger
            .events(parent.id)
            .await
            .unwrap()
            .iter()
            .any(|e| e.event_type == "IssueUnblocked")
    );
    assert_eq!(
        ledger
            .events(question.id)
            .await
            .unwrap()
            .last()
            .unwrap()
            .metadata["answer"],
        "A"
    );
}

#[tokio::test]
async fn backlog_failure_cancel_and_approval() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let human = ledger.ensure_peer("human", PeerKind::Human).await.unwrap();
    let agent = ledger.ensure_peer("agent", PeerKind::Agent).await.unwrap();
    let mut new = NewIssue::task("backlog");
    new.backlog = true;
    let parent = ledger.create_issue(human.id, new).await.unwrap();
    let child = ledger
        .create_child_issue(parent.id, human.id, NewIssue::task("dependency"), true)
        .await
        .unwrap();
    assert_eq!(
        ledger.get_issue(parent.id).await.unwrap().status,
        IssueStatus::Backlog
    );
    assert_eq!(
        ledger.make_ready(parent.id, human.id).await.unwrap().status,
        IssueStatus::Blocked
    );
    let claim = ledger.claim_issue(child.id, agent.id).await.unwrap();
    ledger
        .fail_attempt(claim.attempt.id, "executor error")
        .await
        .unwrap();
    assert_eq!(
        ledger.get_issue(parent.id).await.unwrap().status,
        IssueStatus::Blocked
    );
    ledger
        .cancel_issue(parent.id, human.id, "abandoned")
        .await
        .unwrap();
    assert_eq!(
        ledger.get_issue(parent.id).await.unwrap().status,
        IssueStatus::Cancelled
    );
    let mut approval = NewIssue::task("approve?");
    approval.kind = IssueKind::Approval;
    let approval = ledger.create_issue(human.id, approval).await.unwrap();
    assert!(matches!(
        ledger.complete_issue(approval.id, human.id).await,
        Err(Error::InvalidTransition(IssueStatus::WaitingForHuman))
    ));
    ledger
        .resolve_human_issue(approval.id, human.id, "approved")
        .await
        .unwrap();
    assert!(matches!(
        ledger
            .resolve_human_issue(approval.id, human.id, "again")
            .await,
        Err(Error::InvalidTransition(IssueStatus::Completed))
    ));
}

#[tokio::test]
async fn concurrent_cycle_and_completion_races() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let human = ledger.ensure_peer("human", PeerKind::Human).await.unwrap();
    let a = ledger
        .create_issue(human.id, NewIssue::task("a"))
        .await
        .unwrap();
    let b = ledger
        .create_issue(human.id, NewIssue::task("b"))
        .await
        .unwrap();
    let (left, right) = tokio::join!(
        ledger.add_dependency(a.id, b.id, human.id),
        ledger.add_dependency(b.id, a.id, human.id)
    );
    assert_ne!(left.is_ok(), right.is_ok());
    let parent = ledger
        .create_issue(human.id, NewIssue::task("parent"))
        .await
        .unwrap();
    let child = ledger
        .create_issue(human.id, NewIssue::task("child"))
        .await
        .unwrap();
    let (add, finish) = tokio::join!(
        ledger.add_dependency(parent.id, child.id, human.id),
        ledger.complete_issue(child.id, human.id)
    );
    add.unwrap();
    finish.unwrap();
    assert_eq!(
        ledger.get_issue(parent.id).await.unwrap().status,
        IssueStatus::Ready
    );
}

async fn expire(pool: &sqlx::PgPool, id: ledger::AttemptId) {
    sqlx::query("UPDATE execution_attempts SET lease_expires_at=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(id)
        .execute(pool)
        .await
        .expect("force attempt expiry");
}

#[tokio::test]
async fn attempts_are_unique_and_heartbeats_renew_the_lease() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone())
        .with_lease_duration(std::time::Duration::from_secs(15))
        .unwrap();
    let human = ledger.ensure_peer("human", PeerKind::Human).await.unwrap();
    let agent = ledger.ensure_peer("agent", PeerKind::Agent).await.unwrap();
    let issue = ledger
        .create_issue(human.id, NewIssue::task("work"))
        .await
        .unwrap();
    let claim = ledger.claim_issue(issue.id, agent.id).await.unwrap();
    assert_ne!(claim.attempt.id.0, issue.id.0);
    assert_eq!(claim.attempt.issue_id, issue.id);
    assert_eq!(claim.attempt.peer_id, agent.id);
    assert_eq!(claim.attempt.state, ledger::AttemptState::Claimed);
    assert!(claim.attempt.started_at.is_none());
    assert!(claim.attempt.finished_at.is_none());
    assert!(claim.attempt.lease_expires_at > claim.attempt.heartbeat_at);
    assert!(matches!(
        ledger.complete_attempt(claim.attempt.id).await,
        Err(Error::InvalidTransition(IssueStatus::Claimed))
    ));
    ledger.mark_running(claim.attempt.id).await.unwrap();
    let before_heartbeat = ledger.events(issue.id).await.unwrap().len();
    let renewed = ledger.heartbeat(claim.attempt.id).await.unwrap();
    assert_eq!(
        ledger.events(issue.id).await.unwrap().len(),
        before_heartbeat
    );
    assert_eq!(renewed.state, ledger::AttemptState::Running);
    assert!(renewed.started_at.is_some());
    assert!(renewed.heartbeat_at > claim.attempt.heartbeat_at);
    assert!(renewed.lease_expires_at > claim.attempt.lease_expires_at);
    assert!(ledger.recover_expired_attempts().await.unwrap().is_empty());
    ledger.complete_attempt(claim.attempt.id).await.unwrap();
    let attempts = ledger.attempts(issue.id).await.unwrap();
    assert_eq!(attempts.len(), 1);
    let finished = attempts.first().unwrap();
    assert_eq!(finished.state, ledger::AttemptState::Completed);
    assert!(finished.finished_at.is_some());
    assert_eq!(
        ledger.get_issue(issue.id).await.unwrap().current_attempt_id,
        None
    );
    let events = ledger.events(issue.id).await.unwrap();
    let execution_events = events
        .iter()
        .filter(|event| event.event_type.starts_with("Execution"))
        .collect::<Vec<_>>();
    assert_eq!(
        execution_events
            .iter()
            .map(|event| event.event_type.as_str())
            .collect::<Vec<_>>(),
        vec!["ExecutionClaimed", "ExecutionStarted", "ExecutionCompleted"]
    );
    assert!(
        execution_events
            .iter()
            .all(|event| event.metadata["attempt_id"] == claim.attempt.id.to_string())
    );
}

#[tokio::test]
async fn expired_claimed_and_running_attempts_recover_once() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let human = ledger.ensure_peer("human", PeerKind::Human).await.unwrap();
    let agent = ledger.ensure_peer("agent", PeerKind::Agent).await.unwrap();
    let mut expected = std::collections::HashSet::new();
    for running in [false, true] {
        let issue = ledger
            .create_issue(human.id, NewIssue::task("recover"))
            .await
            .unwrap();
        let claim = ledger.claim_issue(issue.id, agent.id).await.unwrap();
        if running {
            ledger.mark_running(claim.attempt.id).await.unwrap();
        }
        expire(&database.pool, claim.attempt.id).await;
        let before = ledger.events(issue.id).await.unwrap().len();
        assert!(
            matches!(ledger.mark_running(claim.attempt.id).await, Err(Error::ExecutionLost(id)) if id == claim.attempt.id)
        );
        assert!(
            matches!(ledger.complete_attempt(claim.attempt.id).await, Err(Error::ExecutionLost(id)) if id == claim.attempt.id)
        );
        assert!(
            matches!(ledger.fail_attempt(claim.attempt.id, "expired failure").await, Err(Error::ExecutionLost(id)) if id == claim.attempt.id)
        );
        assert!(
            matches!(ledger.create_child_for_attempt(claim.attempt.id, NewIssue::task("expired child"), true).await, Err(Error::ExecutionLost(id)) if id == claim.attempt.id)
        );
        assert!(
            matches!(ledger.heartbeat(claim.attempt.id).await, Err(Error::ExecutionLost(id)) if id == claim.attempt.id)
        );
        assert_eq!(ledger.events(issue.id).await.unwrap().len(), before);
        expected.insert(issue.id);
    }
    let recovered = ledger.recover_expired_attempts().await.unwrap();
    assert_eq!(
        recovered
            .into_iter()
            .collect::<std::collections::HashSet<_>>(),
        expected
    );
    for id in expected {
        let issue = ledger.get_issue(id).await.unwrap();
        assert_eq!(issue.status, IssueStatus::Ready);
        assert_eq!(issue.owner, None);
        assert_eq!(issue.current_attempt_id, None);
        let attempts = ledger.attempts(id).await.unwrap();
        assert_eq!(
            attempts.first().unwrap().state,
            ledger::AttemptState::Expired
        );
        assert!(attempts.first().unwrap().finished_at.is_some());
        let before = ledger.events(id).await.unwrap().len();
        assert!(ledger.recover_expired_attempts().await.unwrap().is_empty());
        assert_eq!(ledger.events(id).await.unwrap().len(), before);
    }
}

#[tokio::test]
async fn a_retry_by_the_same_peer_fences_every_stale_mutation() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let human = ledger.ensure_peer("human", PeerKind::Human).await.unwrap();
    let agent = ledger.ensure_peer("agent", PeerKind::Agent).await.unwrap();
    let issue = ledger
        .create_issue(human.id, NewIssue::task("retry"))
        .await
        .unwrap();
    let prerequisite = ledger
        .create_issue(human.id, NewIssue::task("prerequisite"))
        .await
        .unwrap();
    let first = ledger.claim_issue(issue.id, agent.id).await.unwrap();
    ledger.mark_running(first.attempt.id).await.unwrap();
    expire(&database.pool, first.attempt.id).await;
    assert_eq!(
        ledger.recover_expired_attempts().await.unwrap(),
        vec![issue.id]
    );
    let same_peer = ledger.ensure_peer("agent", PeerKind::Agent).await.unwrap();
    assert_eq!(same_peer.id, agent.id);
    let retry = ledger.claim_issue(issue.id, same_peer.id).await.unwrap();
    assert_ne!(first.attempt.id, retry.attempt.id);
    ledger.mark_running(retry.attempt.id).await.unwrap();
    let before = ledger.events(issue.id).await.unwrap().len();
    assert!(matches!(
        ledger
            .create_issue(agent.id, NewIssue::task("stale root task"))
            .await,
        Err(Error::IneligibleActor)
    ));
    let mut question = NewIssue::task("stale root question");
    question.kind = IssueKind::Question;
    assert!(matches!(
        ledger.create_issue(agent.id, question).await,
        Err(Error::IneligibleActor)
    ));
    assert!(
        matches!(ledger.mark_running(first.attempt.id).await, Err(Error::ExecutionLost(id)) if id == first.attempt.id)
    );
    assert!(
        matches!(ledger.heartbeat(first.attempt.id).await, Err(Error::ExecutionLost(id)) if id == first.attempt.id)
    );
    assert!(
        matches!(ledger.complete_attempt(first.attempt.id).await, Err(Error::ExecutionLost(id)) if id == first.attempt.id)
    );
    assert!(
        matches!(ledger.fail_attempt(first.attempt.id, "stale").await, Err(Error::ExecutionLost(id)) if id == first.attempt.id)
    );
    assert!(
        matches!(ledger.create_child_for_attempt(first.attempt.id, NewIssue::task("stale child"), true).await, Err(Error::ExecutionLost(id)) if id == first.attempt.id)
    );
    assert!(matches!(
        ledger.complete_issue(issue.id, agent.id).await,
        Err(Error::InvalidTransition(IssueStatus::Running))
    ));
    assert!(matches!(
        ledger.complete_issue(issue.id, human.id).await,
        Err(Error::InvalidTransition(IssueStatus::Running))
    ));
    assert!(matches!(
        ledger
            .create_child_issue(issue.id, agent.id, NewIssue::task("bypass"), true)
            .await,
        Err(Error::IneligibleActor)
    ));
    assert!(matches!(
        ledger
            .add_dependency(issue.id, prerequisite.id, agent.id)
            .await,
        Err(Error::IneligibleActor)
    ));
    assert!(matches!(
        ledger.cancel_issue(issue.id, agent.id, "bypass").await,
        Err(Error::IneligibleActor)
    ));
    assert_eq!(ledger.events(issue.id).await.unwrap().len(), before);
    assert!(ledger.children(issue.id).await.unwrap().is_empty());
    assert_eq!(
        ledger.get_issue(issue.id).await.unwrap().current_attempt_id,
        Some(retry.attempt.id)
    );
    ledger.complete_attempt(retry.attempt.id).await.unwrap();
    let attempts = ledger.attempts(issue.id).await.unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(
        attempts.first().unwrap().state,
        ledger::AttemptState::Expired
    );
    assert_eq!(
        attempts.last().unwrap().state,
        ledger::AttemptState::Completed
    );
}

#[tokio::test]
async fn terminal_attempt_updates_are_rejected_without_events() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let human = ledger.ensure_peer("human", PeerKind::Human).await.unwrap();
    let agent = ledger.ensure_peer("agent", PeerKind::Agent).await.unwrap();
    for failed in [false, true] {
        let issue = ledger
            .create_issue(human.id, NewIssue::task("terminal"))
            .await
            .unwrap();
        let claim = ledger.claim_issue(issue.id, agent.id).await.unwrap();
        ledger.mark_running(claim.attempt.id).await.unwrap();
        if failed {
            ledger
                .fail_attempt(claim.attempt.id, "executor failure")
                .await
                .unwrap();
        } else {
            ledger.complete_attempt(claim.attempt.id).await.unwrap();
        }
        let before = ledger.events(issue.id).await.unwrap().len();
        assert!(
            matches!(ledger.complete_attempt(claim.attempt.id).await, Err(Error::ExecutionLost(id)) if id == claim.attempt.id)
        );
        assert!(
            matches!(ledger.fail_attempt(claim.attempt.id, "duplicate").await, Err(Error::ExecutionLost(id)) if id == claim.attempt.id)
        );
        assert_eq!(ledger.events(issue.id).await.unwrap().len(), before);
        assert!(ledger.recover_expired_attempts().await.unwrap().is_empty());
        assert_eq!(
            ledger
                .attempts(issue.id)
                .await
                .unwrap()
                .first()
                .unwrap()
                .state,
            if failed {
                ledger::AttemptState::Failed
            } else {
                ledger::AttemptState::Completed
            }
        );
    }
}

#[tokio::test]
async fn recovery_races_allow_only_one_replacement_attempt() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let human = ledger.ensure_peer("human", PeerKind::Human).await.unwrap();
    let agent = ledger.ensure_peer("agent", PeerKind::Agent).await.unwrap();
    let issue = ledger
        .create_issue(human.id, NewIssue::task("race"))
        .await
        .unwrap();
    let claim = ledger.claim_issue(issue.id, agent.id).await.unwrap();
    expire(&database.pool, claim.attempt.id).await;
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let ledger = ledger.clone();
        let barrier = barrier.clone();
        tasks.spawn(async move {
            barrier.wait().await;
            let recovered = ledger.recover_expired_attempts().await.unwrap();
            let claim = ledger.claim_next_issue(agent.id).await.unwrap();
            (recovered, claim)
        });
    }
    let mut recoveries = 0;
    let mut claims = Vec::new();
    while let Some(result) = tasks.join_next().await {
        let (recovered, claim) = result.unwrap();
        recoveries += recovered.len();
        if let Some(claim) = claim {
            claims.push(claim);
        }
    }
    assert_eq!(recoveries, 1);
    assert_eq!(claims.len(), 1);
    let replacement = claims.first().unwrap();
    assert_eq!(replacement.issue.id, issue.id);
    assert_ne!(replacement.attempt.id, claim.attempt.id);
    assert_eq!(
        ledger.get_issue(issue.id).await.unwrap().current_attempt_id,
        Some(replacement.attempt.id)
    );
    let attempts = ledger.attempts(issue.id).await.unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(
        attempts
            .iter()
            .filter(|attempt| matches!(
                attempt.state,
                ledger::AttemptState::Claimed | ledger::AttemptState::Running
            ))
            .count(),
        1
    );
}

#[tokio::test]
async fn handoff_finishes_attempt_and_unblocks_parent_after_child() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let human = ledger.ensure_peer("human", PeerKind::Human).await.unwrap();
    let agent = ledger.ensure_peer("agent", PeerKind::Agent).await.unwrap();
    for blocking in [false, true] {
        let parent = ledger
            .create_issue(human.id, NewIssue::task("handoff"))
            .await
            .unwrap();
        let claim = ledger.claim_issue(parent.id, agent.id).await.unwrap();
        ledger.mark_running(claim.attempt.id).await.unwrap();
        let child = ledger
            .create_child_for_attempt(claim.attempt.id, NewIssue::task("child"), blocking)
            .await
            .unwrap();
        assert_eq!(child.parent_id, Some(parent.id));
        let parent_state = ledger.get_issue(parent.id).await.unwrap();
        assert_eq!(
            parent_state.status,
            if blocking {
                IssueStatus::Blocked
            } else {
                IssueStatus::Completed
            }
        );
        assert_eq!(parent_state.current_attempt_id, None);
        let attempts = ledger.attempts(parent.id).await.unwrap();
        assert_eq!(
            attempts.first().unwrap().state,
            ledger::AttemptState::Completed
        );
        assert!(attempts.first().unwrap().finished_at.is_some());
        assert!(
            matches!(ledger.complete_attempt(claim.attempt.id).await, Err(Error::ExecutionLost(id)) if id == claim.attempt.id)
        );
        let before = ledger.events(parent.id).await.unwrap().len();
        assert!(
            matches!(ledger.create_child_for_attempt(claim.attempt.id, NewIssue::task("duplicate child"), blocking).await, Err(Error::ExecutionLost(id)) if id == claim.attempt.id)
        );
        assert_eq!(ledger.events(parent.id).await.unwrap().len(), before);
        assert_eq!(ledger.children(parent.id).await.unwrap().len(), 1);
        ledger.complete_issue(child.id, human.id).await.unwrap();
        assert_eq!(
            ledger.get_issue(parent.id).await.unwrap().status,
            if blocking {
                IssueStatus::Ready
            } else {
                IssueStatus::Completed
            }
        );
    }
}

#[tokio::test]
async fn human_cancellation_and_blocking_changes_retire_active_attempts() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let human = ledger.ensure_peer("human", PeerKind::Human).await.unwrap();
    let agent = ledger.ensure_peer("agent", PeerKind::Agent).await.unwrap();
    for change in 0..3 {
        let issue = ledger
            .create_issue(human.id, NewIssue::task("active"))
            .await
            .unwrap();
        let claim = ledger.claim_issue(issue.id, agent.id).await.unwrap();
        ledger.mark_running(claim.attempt.id).await.unwrap();
        let expected = if change == 0 {
            ledger
                .cancel_issue(issue.id, human.id, "operator cancelled")
                .await
                .unwrap();
            IssueStatus::Cancelled
        } else if change == 1 {
            let prerequisite = ledger
                .create_issue(human.id, NewIssue::task("prerequisite"))
                .await
                .unwrap();
            ledger
                .add_dependency(issue.id, prerequisite.id, human.id)
                .await
                .unwrap();
            IssueStatus::Blocked
        } else {
            ledger
                .create_child_issue(issue.id, human.id, NewIssue::task("blocking child"), true)
                .await
                .unwrap();
            IssueStatus::Blocked
        };
        let updated = ledger.get_issue(issue.id).await.unwrap();
        assert_eq!(updated.status, expected);
        assert_eq!(updated.current_attempt_id, None);
        if change != 0 {
            assert_eq!(updated.owner, None);
        }
        let attempts = ledger.attempts(issue.id).await.unwrap();
        assert_eq!(
            attempts.first().unwrap().state,
            ledger::AttemptState::Cancelled
        );
        assert!(attempts.first().unwrap().finished_at.is_some());
        assert!(
            matches!(ledger.heartbeat(claim.attempt.id).await, Err(Error::ExecutionLost(id)) if id == claim.attempt.id)
        );
        assert!(
            matches!(ledger.complete_attempt(claim.attempt.id).await, Err(Error::ExecutionLost(id)) if id == claim.attempt.id)
        );
        assert!(ledger.recover_expired_attempts().await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn database_constraints_enforce_active_attempt_identity() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let human = ledger.ensure_peer("human", PeerKind::Human).await.unwrap();
    let agent = ledger.ensure_peer("agent", PeerKind::Agent).await.unwrap();
    let issue = ledger
        .create_issue(human.id, NewIssue::task("one active"))
        .await
        .unwrap();
    let other = ledger
        .create_issue(human.id, NewIssue::task("other"))
        .await
        .unwrap();
    let claim = ledger.claim_issue(issue.id, agent.id).await.unwrap();
    let other_claim = ledger.claim_issue(other.id, agent.id).await.unwrap();
    for expired in [false, true] {
        if expired {
            expire(&database.pool, claim.attempt.id).await;
        }
        let duplicate = sqlx::query("INSERT INTO execution_attempts(id,issue_id,peer_id,state,lease_expires_at) VALUES(gen_random_uuid(),$1,$2,'Claimed',clock_timestamp()+interval '30 seconds')")
            .bind(issue.id).bind(agent.id).execute(&database.pool).await;
        assert!(
            matches!(duplicate, Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("23505"))
        );
    }
    let wrong_issue = sqlx::query("UPDATE issues SET current_attempt_id=$2 WHERE id=$1")
        .bind(issue.id)
        .bind(other_claim.attempt.id)
        .execute(&database.pool)
        .await;
    assert!(
        matches!(wrong_issue, Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("23503"))
    );
    let missing_pointer =
        sqlx::query("UPDATE issues SET status='Running',current_attempt_id=NULL WHERE id=$1")
            .bind(issue.id)
            .execute(&database.pool)
            .await;
    assert!(
        matches!(missing_pointer, Err(sqlx::Error::Database(error)) if error.code().as_deref() == Some("23514"))
    );
    assert_eq!(ledger.attempts(issue.id).await.unwrap().len(), 1);
    assert_eq!(
        ledger.get_issue(issue.id).await.unwrap().current_attempt_id,
        Some(claim.attempt.id)
    );
}

#[tokio::test]
async fn recovery_and_heartbeat_races_respect_database_expiry() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let human = ledger.ensure_peer("human", PeerKind::Human).await.unwrap();
    let agent = ledger.ensure_peer("agent", PeerKind::Agent).await.unwrap();
    for expired in [false, true] {
        let issue = ledger
            .create_issue(human.id, NewIssue::task("heartbeat race"))
            .await
            .unwrap();
        let claim = ledger.claim_issue(issue.id, agent.id).await.unwrap();
        ledger.mark_running(claim.attempt.id).await.unwrap();
        if expired {
            expire(&database.pool, claim.attempt.id).await;
        }
        let (heartbeat, recovered) = tokio::join!(
            ledger.heartbeat(claim.attempt.id),
            ledger.recover_expired_attempts()
        );
        let recovered = recovered.unwrap();
        if expired {
            assert!(matches!(heartbeat, Err(Error::ExecutionLost(id)) if id == claim.attempt.id));
            assert_eq!(recovered, vec![issue.id]);
            assert_eq!(
                ledger.get_issue(issue.id).await.unwrap().status,
                IssueStatus::Ready
            );
            // Finish the recovered issue so it cannot affect the next case's recovery.
            ledger.complete_issue(issue.id, human.id).await.unwrap();
        } else {
            assert!(heartbeat.unwrap().lease_expires_at > claim.attempt.lease_expires_at);
            assert!(recovered.is_empty());
            assert_eq!(
                ledger.get_issue(issue.id).await.unwrap().current_attempt_id,
                Some(claim.attempt.id)
            );
            ledger.complete_attempt(claim.attempt.id).await.unwrap();
        }
    }
}

#[tokio::test]
async fn migration_adopts_and_recovers_existing_stranded_execution() {
    let database = TestDatabase::start().await.unwrap();
    sqlx::query("CREATE SCHEMA legacy")
        .execute(&database.pool)
        .await
        .unwrap();
    let legacy_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(2)
        .after_connect(|connection, _metadata| {
            Box::pin(async move {
                sqlx::query("SET search_path TO legacy")
                    .execute(connection)
                    .await?;
                Ok(())
            })
        })
        .connect_with(database.pool.connect_options().as_ref().clone())
        .await
        .unwrap();
    sqlx::raw_sql(include_str!("../../../migrations/0001_ledger.sql"))
        .execute(&legacy_pool)
        .await
        .unwrap();
    let agent: ledger::PeerId = sqlx::query_scalar("INSERT INTO peers(id,name,kind) VALUES(gen_random_uuid(),'legacy agent','Agent') RETURNING id")
        .fetch_one(&legacy_pool).await.unwrap();
    let mut issues = Vec::new();
    for status in [IssueStatus::Claimed, IssueStatus::Running] {
        let id: ledger::IssueId = sqlx::query_scalar("INSERT INTO issues(id,title,kind,status,created_by,owner) VALUES(gen_random_uuid(),'stranded','Task',$1,$2,$2) RETURNING id")
            .bind(status).bind(agent).fetch_one(&legacy_pool).await.unwrap();
        issues.push(id);
    }
    sqlx::raw_sql(include_str!(
        "../../../migrations/0002_execution_attempts.sql"
    ))
    .execute(&legacy_pool)
    .await
    .unwrap();
    let ledger = Ledger::from_pool(legacy_pool);
    for id in &issues {
        let issue = ledger.get_issue(*id).await.unwrap();
        let attempts = ledger.attempts(*id).await.unwrap();
        assert_eq!(attempts.len(), 1);
        let attempt = attempts.first().unwrap();
        assert_eq!(issue.current_attempt_id, Some(attempt.id));
        assert_eq!(attempt.peer_id, agent);
        assert_eq!(
            attempt.started_at.is_some(),
            issue.status == IssueStatus::Running
        );
        assert!(
            matches!(ledger.heartbeat(attempt.id).await, Err(Error::ExecutionLost(lost)) if lost == attempt.id)
        );
        let events = ledger.events(*id).await.unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.event_type == "ExecutionClaimed"
                    && event.metadata["migration"] == true)
        );
    }
    let recovered = ledger.recover_expired_attempts().await.unwrap();
    assert_eq!(
        recovered
            .into_iter()
            .collect::<std::collections::HashSet<_>>(),
        issues.iter().copied().collect()
    );
    for id in issues {
        let issue = ledger.get_issue(id).await.unwrap();
        assert_eq!(issue.status, IssueStatus::Ready);
        assert_eq!(issue.current_attempt_id, None);
        assert_eq!(
            ledger.attempts(id).await.unwrap().first().unwrap().state,
            ledger::AttemptState::Expired
        );
        let retry = ledger.claim_issue(id, agent).await.unwrap();
        assert_eq!(ledger.attempts(id).await.unwrap().len(), 2);
        ledger.mark_running(retry.attempt.id).await.unwrap();
        ledger.complete_attempt(retry.attempt.id).await.unwrap();
    }
}
