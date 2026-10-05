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
    let agent = ledger.ensure_peer("worker", PeerKind::Agent).await.unwrap();
    let mut issue = NewIssue::task("Cannot execute");
    issue.description = "fail: missing capability".to_owned();
    let issue = ledger.create_issue(agent.id, issue).await.unwrap();
    let worker = Worker::new(ledger.clone(), Agent { peer_id: agent.id }, FakeExecutor);
    let failed = worker.tick().await.unwrap().expect("expected failure");
    assert_eq!(failed.id, issue.id);
    assert_eq!(failed.status, IssueStatus::Failed);
    assert!(worker.tick().await.unwrap().is_none());
    assert!(ledger.events(issue.id).await.unwrap().len() >= 4);
}
