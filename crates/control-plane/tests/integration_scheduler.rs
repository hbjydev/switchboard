use std::{collections::HashSet, sync::Arc, time::Duration};

use control_plane::scheduler::{self, SchedulerConfig};
use ledger::{AttemptId, AttemptState, IssueId, IssueStatus, Ledger, NewIssue, PeerId, PeerKind};
use runtime::{
    Agent, AgentBackend, BackendError, CancellationToken, ExecutionRequest, ExecutionResult,
    FakeExecutor, Worker,
};
use tokio::sync::{Semaphore, mpsc};

#[path = "../../../tests/support/mod.rs"]
mod support;
use support::TestDatabase;

const fn config(concurrency: usize) -> SchedulerConfig {
    SchedulerConfig {
        concurrency,
        interval: Duration::from_millis(20),
        shutdown_grace: Duration::from_secs(2),
    }
}

async fn peers(ledger: &Ledger) -> (PeerId, PeerId) {
    let human = ledger
        .ensure_peer("operator", PeerKind::Human)
        .await
        .expect("create test operator");
    let agent = ledger
        .ensure_peer("worker", PeerKind::Agent)
        .await
        .expect("create test agent");
    (human.id, agent.id)
}

async fn wait_completed(ledger: &Ledger, ids: &[IssueId]) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let mut all_completed = true;
            for id in ids {
                all_completed &= ledger
                    .get_issue(*id)
                    .await
                    .expect("read scheduled issue")
                    .status
                    == IssueStatus::Completed;
            }
            if all_completed {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("scheduled issues complete within deadline");
}

#[tokio::test]
async fn persistent_scheduler_discovers_new_work_and_completes_through_fake_backend() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let (human, agent) = peers(&ledger).await;
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(scheduler::run(
        Worker::new(ledger.clone(), Agent { peer_id: agent }, FakeExecutor),
        config(1),
        shutdown.clone(),
    ));
    let issue = ledger
        .create_issue(human, NewIssue::task("persistent work"))
        .await
        .unwrap();
    wait_completed(&ledger, &[issue.id]).await;
    shutdown.cancel();
    task.await.unwrap().unwrap();
    let attempts = ledger.attempts(issue.id).await.unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts.first().unwrap().state, AttemptState::Completed);
}

struct ControlledBackend {
    started: mpsc::UnboundedSender<(IssueId, AttemptId)>,
    cancelled: mpsc::UnboundedSender<AttemptId>,
    release: Arc<Semaphore>,
}

impl AgentBackend for ControlledBackend {
    async fn execute(
        &self,
        request: ExecutionRequest,
        cancellation: CancellationToken,
    ) -> Result<ExecutionResult, BackendError> {
        self.started
            .send((request.issue.id, request.attempt_id))
            .expect("notify test of execution start");
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                self.cancelled.send(request.attempt_id).expect("notify test of cancellation");
                // Even an incorrect late completion must not be applied on shutdown.
                Ok(ExecutionResult::Completed { summary: "late completion".into() })
            }
            permit = self.release.acquire() => {
                permit.expect("test semaphore remains open").forget();
                Ok(ExecutionResult::Completed { summary: "completed".into() })
            }
        }
    }
}

async fn receive<T>(receiver: &mut mpsc::UnboundedReceiver<T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), receiver.recv())
        .await
        .expect("backend signal arrives within deadline")
        .expect("backend signal channel remains open")
}

#[tokio::test]
async fn scheduler_concurrency_claims_distinct_issues_and_attempts() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let (human, agent) = peers(&ledger).await;
    let mut issues = Vec::new();
    for index in 0..12 {
        issues.push(
            ledger
                .create_issue(human, NewIssue::task(format!("task {index}")))
                .await
                .unwrap()
                .id,
        );
    }
    let (started, mut starts) = mpsc::unbounded_channel();
    let (cancelled, _cancellations) = mpsc::unbounded_channel();
    let release = Arc::new(Semaphore::new(0));
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(scheduler::run(
        Worker::new(
            ledger.clone(),
            Agent { peer_id: agent },
            ControlledBackend {
                started,
                cancelled,
                release: release.clone(),
            },
        ),
        config(4),
        shutdown.clone(),
    ));
    let mut claimed = HashSet::new();
    let mut attempts = HashSet::new();
    for _ in 0..4 {
        let (issue, attempt) = receive(&mut starts).await;
        assert!(claimed.insert(issue));
        assert!(attempts.insert(attempt));
    }
    // All four lanes reached the backend before any execution was released.
    release.add_permits(12);
    for _ in 4..12 {
        let (issue, attempt) = receive(&mut starts).await;
        assert!(claimed.insert(issue));
        assert!(attempts.insert(attempt));
    }
    wait_completed(&ledger, &issues).await;
    shutdown.cancel();
    task.await.unwrap().unwrap();
    for issue in issues {
        assert_eq!(ledger.attempts(issue).await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn shutdown_cancels_execution_stops_claims_and_restart_recovers_expired_work() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone())
        .with_lease_duration(Duration::from_millis(900))
        .unwrap();
    let (human, agent) = peers(&ledger).await;
    let first = ledger
        .create_issue(human, NewIssue::task("interrupted"))
        .await
        .unwrap();
    let (started, mut starts) = mpsc::unbounded_channel();
    let (cancelled, mut cancellations) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(scheduler::run(
        Worker::new(
            ledger.clone(),
            Agent { peer_id: agent },
            ControlledBackend {
                started,
                cancelled,
                release: Arc::new(Semaphore::new(0)),
            },
        ),
        config(1),
        shutdown.clone(),
    ));
    let (issue, attempt) = receive(&mut starts).await;
    assert_eq!(issue, first.id);
    let second = ledger
        .create_issue(human, NewIssue::task("awaits restart"))
        .await
        .unwrap();
    shutdown.cancel();
    assert_eq!(receive(&mut cancellations).await, attempt);
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(
        ledger.get_issue(first.id).await.unwrap().status,
        IssueStatus::Running
    );
    assert_eq!(
        ledger.get_issue(second.id).await.unwrap().status,
        IssueStatus::Ready
    );
    let heartbeat = ledger
        .attempts(first.id)
        .await
        .unwrap()
        .first()
        .unwrap()
        .heartbeat_at;
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(
        ledger
            .attempts(first.id)
            .await
            .unwrap()
            .first()
            .unwrap()
            .heartbeat_at,
        heartbeat
    );
    sqlx::query("UPDATE execution_attempts SET lease_expires_at=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(attempt).execute(&database.pool).await.unwrap();
    let restarted_shutdown = CancellationToken::new();
    let restarted = tokio::spawn(scheduler::run(
        Worker::new(ledger.clone(), Agent { peer_id: agent }, FakeExecutor),
        config(1),
        restarted_shutdown.clone(),
    ));
    wait_completed(&ledger, &[first.id, second.id]).await;
    restarted_shutdown.cancel();
    restarted.await.unwrap().unwrap();
    let attempts = ledger.attempts(first.id).await.unwrap();
    assert_eq!(attempts.len(), 2);
    assert!(
        attempts
            .iter()
            .any(|item| item.id == attempt && item.state == AttemptState::Expired)
    );
    assert!(
        attempts
            .iter()
            .any(|item| item.id != attempt && item.state == AttemptState::Completed)
    );
}

#[tokio::test]
async fn already_cancelled_scheduler_never_claims_work() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let (human, agent) = peers(&ledger).await;
    let issue = ledger
        .create_issue(human, NewIssue::task("unclaimed"))
        .await
        .unwrap();
    let shutdown = CancellationToken::new();
    shutdown.cancel();
    scheduler::run(
        Worker::new(ledger.clone(), Agent { peer_id: agent }, FakeExecutor),
        config(4),
        shutdown,
    )
    .await
    .unwrap();
    assert_eq!(
        ledger.get_issue(issue.id).await.unwrap().status,
        IssueStatus::Ready
    );
    assert!(ledger.attempts(issue.id).await.unwrap().is_empty());
}

#[tokio::test]
async fn shutdown_while_recovery_waits_on_database_lock_prevents_later_claim() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let (human, agent) = peers(&ledger).await;
    let issue = ledger
        .create_issue(human, NewIssue::task("still ready after lock releases"))
        .await
        .unwrap();
    let mut transaction = database.pool.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock(741932)")
        .execute(&mut *transaction)
        .await
        .unwrap();
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(scheduler::run(
        Worker::new(ledger.clone(), Agent { peer_id: agent }, FakeExecutor),
        config(1),
        shutdown.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_locks WHERE locktype='advisory' AND NOT granted)",
            )
            .fetch_one(&database.pool)
            .await
            .unwrap();
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    transaction.rollback().await.unwrap();
    assert_eq!(
        ledger.get_issue(issue.id).await.unwrap().status,
        IssueStatus::Ready
    );
    assert!(ledger.attempts(issue.id).await.unwrap().is_empty());
}

struct UncooperativeBackend {
    started: mpsc::UnboundedSender<AttemptId>,
    dropped: mpsc::UnboundedSender<()>,
}

struct DropSignal(mpsc::UnboundedSender<()>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

impl AgentBackend for UncooperativeBackend {
    async fn execute(
        &self,
        request: ExecutionRequest,
        _cancellation: CancellationToken,
    ) -> Result<ExecutionResult, BackendError> {
        let _guard = DropSignal(self.dropped.clone());
        self.started
            .send(request.attempt_id)
            .expect("notify test of execution start");
        std::future::pending().await
    }
}

#[tokio::test]
async fn shutdown_bounds_cleanup_for_uncooperative_backend() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let (human, agent) = peers(&ledger).await;
    let issue = ledger
        .create_issue(human, NewIssue::task("uncooperative"))
        .await
        .unwrap();
    let (started, mut starts) = mpsc::unbounded_channel();
    let (dropped, mut drops) = mpsc::unbounded_channel();
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(scheduler::run(
        Worker::new(
            ledger.clone(),
            Agent { peer_id: agent },
            UncooperativeBackend { started, dropped },
        ),
        SchedulerConfig {
            shutdown_grace: Duration::from_millis(50),
            ..config(1)
        },
        shutdown.clone(),
    ));
    let attempt = receive(&mut starts).await;
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    receive(&mut drops).await;
    let attempts = ledger.attempts(issue.id).await.unwrap();
    assert_eq!(attempts.first().unwrap().id, attempt);
    assert_eq!(attempts.first().unwrap().state, AttemptState::Running);
}
