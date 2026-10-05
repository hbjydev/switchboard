use ledger::{IssueKind, IssueStatus, Ledger, NewIssue, PeerKind};
use runtime::{Agent, FakeExecutor, Worker};
use sqlx::PgPool;

#[sqlx::test(migrations = "../../migrations")]
async fn demo_survives_worker_restarts(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let ledger = Ledger::from_pool(pool);
    let human = ledger.ensure_peer("operator", PeerKind::Human).await?;
    let agent = ledger.ensure_peer("demo-worker", PeerKind::Agent).await?;
    let mut new = NewIssue::task("Build demo feature");
    new.description = "demo".to_owned();
    let parent = ledger.create_issue(human.id, new).await?;
    let worker = Worker::new(ledger.clone(), Agent { peer_id: agent.id }, FakeExecutor);

    let blocked = worker.tick().await?.ok_or("expected parent")?;
    assert_eq!(blocked.id, parent.id);
    assert_eq!(blocked.status, IssueStatus::Blocked);
    let child = worker.tick().await?.ok_or("expected child")?;
    assert_eq!(child.parent_id, Some(parent.id));
    assert_eq!(child.status, IssueStatus::Completed);
    assert_eq!(
        ledger.get_issue(parent.id).await?.status,
        IssueStatus::Ready
    );
    let waiting = worker.tick().await?.ok_or("expected resumed parent")?;
    assert_eq!(waiting.status, IssueStatus::WaitingForHuman);
    assert!(worker.tick().await?.is_none());

    let children = ledger.children(parent.id).await?;
    assert_eq!(children.len(), 2);
    let question = children
        .iter()
        .find(|child| child.kind == IssueKind::Question)
        .ok_or("missing question")?;
    assert_eq!(question.status, IssueStatus::WaitingForHuman);
    ledger
        .resolve_human_issue(question.id, human.id, "Use option A")
        .await?;
    assert_eq!(
        ledger.get_issue(parent.id).await?.status,
        IssueStatus::Ready
    );
    let restarted = Worker::new(ledger.clone(), Agent { peer_id: agent.id }, FakeExecutor);
    let completed = restarted.tick().await?.ok_or("expected final parent")?;
    assert_eq!(completed.id, parent.id);
    assert_eq!(completed.status, IssueStatus::Completed);
    assert_eq!(ledger.children(parent.id).await?.len(), 2);
    assert!(restarted.tick().await?.is_none());
    assert!(ledger.events(parent.id).await?.len() >= 10);
    Ok(())
}

#[sqlx::test(migrations = "../../migrations")]
async fn executor_failure_is_persisted(pool: PgPool) -> Result<(), Box<dyn std::error::Error>> {
    let ledger = Ledger::from_pool(pool);
    let agent = ledger.ensure_peer("worker", PeerKind::Agent).await?;
    let mut issue = NewIssue::task("Cannot execute");
    issue.description = "fail: missing capability".to_owned();
    let issue = ledger.create_issue(agent.id, issue).await?;
    let worker = Worker::new(ledger.clone(), Agent { peer_id: agent.id }, FakeExecutor);
    let failed = worker.tick().await?.ok_or("expected failure")?;
    assert_eq!(failed.id, issue.id);
    assert_eq!(failed.status, IssueStatus::Failed);
    assert!(worker.tick().await?.is_none());
    assert!(ledger.events(issue.id).await?.len() >= 4);
    Ok(())
}
