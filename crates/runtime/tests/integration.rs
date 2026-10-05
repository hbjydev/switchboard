use ledger::{IssueKind, IssueStatus, Ledger, NewIssue, PeerKind};
use runtime::{Agent, FakeExecutor, Worker};
#[path = "../../../tests/support/mod.rs"]
mod support;
use support::TestDatabase;

#[tokio::test]
async fn demo_survives_worker_restarts() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let human = ledger
        .ensure_peer("operator", PeerKind::Human)
        .await
        .unwrap();
    let agent = ledger
        .ensure_peer("demo-worker", PeerKind::Agent)
        .await
        .unwrap();
    let mut new = NewIssue::task("Build demo feature");
    new.description = "demo".to_owned();
    let parent = ledger.create_issue(human.id, new).await.unwrap();
    let worker = Worker::new(ledger.clone(), Agent { peer_id: agent.id }, FakeExecutor);

    let blocked = worker.tick().await.unwrap().expect("expected parent");
    assert_eq!(blocked.id, parent.id);
    assert_eq!(blocked.status, IssueStatus::Blocked);
    let child = worker.tick().await.unwrap().expect("expected child");
    assert_eq!(child.parent_id, Some(parent.id));
    assert_eq!(child.status, IssueStatus::Completed);
    assert_eq!(
        ledger.get_issue(parent.id).await.unwrap().status,
        IssueStatus::Ready
    );
    let waiting = worker
        .tick()
        .await
        .unwrap()
        .expect("expected resumed parent");
    assert_eq!(waiting.status, IssueStatus::WaitingForHuman);
    assert!(worker.tick().await.unwrap().is_none());

    let children = ledger.children(parent.id).await.unwrap();
    assert_eq!(children.len(), 2);
    let question = children
        .iter()
        .find(|child| child.kind == IssueKind::Question)
        .expect("missing question");
    assert_eq!(question.status, IssueStatus::WaitingForHuman);
    ledger
        .resolve_human_issue(question.id, human.id, "Use option A")
        .await
        .unwrap();
    assert_eq!(
        ledger.get_issue(parent.id).await.unwrap().status,
        IssueStatus::Ready
    );
    let restarted = Worker::new(ledger.clone(), Agent { peer_id: agent.id }, FakeExecutor);
    let completed = restarted
        .tick()
        .await
        .unwrap()
        .expect("expected final parent");
    assert_eq!(completed.id, parent.id);
    assert_eq!(completed.status, IssueStatus::Completed);
    assert_eq!(ledger.children(parent.id).await.unwrap().len(), 2);
    assert!(restarted.tick().await.unwrap().is_none());
    assert!(ledger.events(parent.id).await.unwrap().len() >= 10);
}

#[tokio::test]
async fn executor_failure_is_persisted() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let human = ledger.ensure_peer("human", PeerKind::Human).await.unwrap();
    let agent = ledger.ensure_peer("worker", PeerKind::Agent).await.unwrap();
    let mut issue = NewIssue::task("Cannot execute");
    issue.description = "fail: missing capability".to_owned();
    let issue = ledger.create_issue(human.id, issue).await.unwrap();
    let worker = Worker::new(ledger.clone(), Agent { peer_id: agent.id }, FakeExecutor);
    let failed = worker.tick().await.unwrap().expect("expected failure");
    assert_eq!(failed.id, issue.id);
    assert_eq!(failed.status, IssueStatus::Failed);
    assert!(worker.tick().await.unwrap().is_none());
    assert!(ledger.events(issue.id).await.unwrap().len() >= 4);
}

struct ControlledExecutor {
    started: tokio::sync::mpsc::UnboundedSender<ledger::AttemptId>,
    release: std::sync::Arc<tokio::sync::Notify>,
    dropped: std::sync::Arc<std::sync::atomic::AtomicBool>,
    returned: std::sync::Arc<std::sync::atomic::AtomicBool>,
    outcome: u8,
}

struct ExecutionGuard(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl Drop for ExecutionGuard {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

impl runtime::Executor for ControlledExecutor {
    async fn execute(
        &self,
        issue: &ledger::Issue,
        context: runtime::ExecutionContext,
    ) -> runtime::ExecutionOutcome {
        let _guard = ExecutionGuard(self.dropped.clone());
        assert_eq!(issue.current_attempt_id, Some(context.attempt_id));
        self.started
            .send(context.attempt_id)
            .expect("notify test that execution began");
        self.release.notified().await;
        self.returned
            .store(true, std::sync::atomic::Ordering::SeqCst);
        match self.outcome {
            1 => runtime::ExecutionOutcome::Failed {
                reason: "stale failure".to_owned(),
            },
            2 => runtime::ExecutionOutcome::CreatedChildWork {
                issue: NewIssue::task("stale child"),
                blocking: true,
            },
            3 => runtime::ExecutionOutcome::NeedsHumanInput {
                title: "stale question".to_owned(),
                description: "must not persist".to_owned(),
            },
            _ => runtime::ExecutionOutcome::Completed,
        }
    }
}

#[tokio::test]
async fn worker_renews_a_lease_multiple_times_during_execution() {
    let database = TestDatabase::start().await.unwrap();
    let duration = std::time::Duration::from_millis(900);
    let ledger = Ledger::from_pool(database.pool.clone())
        .with_lease_duration(duration)
        .unwrap();
    let human = ledger.ensure_peer("human", PeerKind::Human).await.unwrap();
    let agent = ledger.ensure_peer("worker", PeerKind::Agent).await.unwrap();
    let issue = ledger
        .create_issue(human.id, NewIssue::task("long execution"))
        .await
        .unwrap();
    let (started, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let returned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker = Worker::new(
        ledger.clone(),
        Agent { peer_id: agent.id },
        ControlledExecutor {
            started,
            release: release.clone(),
            dropped,
            returned: returned.clone(),
            outcome: 0,
        },
    );
    let task = tokio::spawn(async move { worker.tick().await });
    let attempt_id = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    let initial = ledger
        .attempts(issue.id)
        .await
        .unwrap()
        .first()
        .unwrap()
        .clone();
    assert_eq!(attempt_id, initial.id);
    let start = std::time::Instant::now();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut previous = initial.heartbeat_at;
        let mut renewals = 0;
        while renewals < 3 || start.elapsed() <= duration {
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            let attempts = ledger.attempts(issue.id).await.unwrap();
            let attempt = attempts.first().unwrap();
            assert_eq!(attempt.state, ledger::AttemptState::Running);
            if attempt.heartbeat_at > previous {
                previous = attempt.heartbeat_at;
                renewals += 1;
                assert!(attempt.lease_expires_at > initial.lease_expires_at);
            }
            assert!(ledger.recover_expired_attempts().await.unwrap().is_empty());
        }
    })
    .await
    .unwrap();
    assert!(!task.is_finished());
    assert!(!returned.load(std::sync::atomic::Ordering::SeqCst));
    release.notify_one();
    let completed = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(completed.status, IssueStatus::Completed);
    assert_eq!(completed.current_attempt_id, None);
    assert_eq!(
        ledger
            .attempts(issue.id)
            .await
            .unwrap()
            .first()
            .unwrap()
            .state,
        ledger::AttemptState::Completed
    );
}

#[tokio::test]
async fn lost_lease_cancels_the_in_flight_executor() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone())
        .with_lease_duration(std::time::Duration::from_millis(900))
        .unwrap();
    let human = ledger.ensure_peer("human", PeerKind::Human).await.unwrap();
    let agent = ledger.ensure_peer("worker", PeerKind::Agent).await.unwrap();
    let issue = ledger
        .create_issue(human.id, NewIssue::task("interrupted"))
        .await
        .unwrap();
    let (started, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let returned = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker = Worker::new(
        ledger.clone(),
        Agent { peer_id: agent.id },
        ControlledExecutor {
            started,
            release: std::sync::Arc::new(tokio::sync::Notify::new()),
            dropped: dropped.clone(),
            returned: returned.clone(),
            outcome: 0,
        },
    );
    let task = tokio::spawn(async move { worker.tick().await });
    let attempt_id = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    sqlx::query("UPDATE execution_attempts SET lease_expires_at=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(attempt_id).execute(&database.pool).await.unwrap();
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(result, Err(ledger::Error::ExecutionLost(id)) if id == attempt_id));
    assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
    assert!(!returned.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(
        ledger.recover_expired_attempts().await.unwrap(),
        vec![issue.id]
    );
    assert_eq!(
        ledger.get_issue(issue.id).await.unwrap().status,
        IssueStatus::Ready
    );
    assert!(ledger.children(issue.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn stale_worker_outcomes_cannot_mutate_a_retry_by_the_same_peer() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let human = ledger.ensure_peer("human", PeerKind::Human).await.unwrap();
    let agent = ledger.ensure_peer("worker", PeerKind::Agent).await.unwrap();
    for outcome in 0..4 {
        let issue = ledger
            .create_issue(human.id, NewIssue::task("retry"))
            .await
            .unwrap();
        let (started, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let release = std::sync::Arc::new(tokio::sync::Notify::new());
        let worker = Worker::new(
            ledger.clone(),
            Agent { peer_id: agent.id },
            ControlledExecutor {
                started,
                release: release.clone(),
                dropped: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                returned: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                outcome,
            },
        );
        let task = tokio::spawn(async move { worker.tick().await });
        let stale_id = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        sqlx::query("UPDATE execution_attempts SET lease_expires_at=clock_timestamp()-interval '1 second' WHERE id=$1")
            .bind(stale_id).execute(&database.pool).await.unwrap();
        assert_eq!(
            ledger.recover_expired_attempts().await.unwrap(),
            vec![issue.id]
        );
        let retry = ledger.claim_issue(issue.id, agent.id).await.unwrap();
        ledger.mark_running(retry.attempt.id).await.unwrap();
        let before = ledger.events(issue.id).await.unwrap().len();
        release.notify_one();
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(result, Err(ledger::Error::ExecutionLost(id)) if id == stale_id));
        assert_eq!(ledger.events(issue.id).await.unwrap().len(), before);
        assert!(ledger.children(issue.id).await.unwrap().is_empty());
        let current = ledger.get_issue(issue.id).await.unwrap();
        assert_eq!(current.status, IssueStatus::Running);
        assert_eq!(current.current_attempt_id, Some(retry.attempt.id));
        ledger.complete_attempt(retry.attempt.id).await.unwrap();
    }
}
