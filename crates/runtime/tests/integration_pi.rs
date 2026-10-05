#![cfg(all(unix, feature = "rpc-fixture"))]
mod rpc_support;
#[path = "../../../tests/support/mod.rs"]
mod support;

use ledger::{IssueStatus, Ledger, NewIssue, PeerKind};
use rpc_support::Fixture;
use runtime::environment::LocalProcessEnvironment;
use runtime::{Agent, PiBackend, Worker};
use support::TestDatabase;

#[tokio::test]
async fn worker_passes_durable_context_and_persists_pi_summary() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let human = ledger.ensure_peer("human", PeerKind::Human).await.unwrap();
    let peer = ledger.ensure_peer("worker", PeerKind::Agent).await.unwrap();
    let mut parent = NewIssue::task("Parent context");
    parent.backlog = true;
    let parent = ledger.create_issue(human.id, parent).await.unwrap();
    let target = ledger
        .create_child_issue(parent.id, human.id, NewIssue::task("Target task"), false)
        .await
        .unwrap();
    let mut child = NewIssue::task("Existing child");
    child.backlog = true;
    ledger
        .create_child_issue(target.id, human.id, child, false)
        .await
        .unwrap();
    let dependency = ledger
        .create_issue(human.id, NewIssue::task("Completed prerequisite"))
        .await
        .unwrap();
    ledger
        .add_dependency(target.id, dependency.id, human.id)
        .await
        .unwrap();
    ledger
        .complete_issue(dependency.id, human.id)
        .await
        .unwrap();
    let fixture = Fixture::new("normal");
    let worker = Worker::new(
        ledger.clone(),
        Agent { peer_id: peer.id },
        fixture.backend(),
    )
    .with_execution_settings(
        "Repository-specific instructions".into(),
        Some(fixture.workspace.clone()),
    );
    let completed = worker.tick().await.unwrap().unwrap();
    assert_eq!(completed.id, target.id);
    assert_eq!(completed.status, IssueStatus::Completed);
    let prompt: serde_json::Value = serde_json::from_str(&fixture.read("prompt")).unwrap();
    let text = prompt.get("message").unwrap().as_str().unwrap();
    for expected in [
        "Parent context",
        "Existing child",
        "Completed prerequisite",
        "Repository-specific instructions",
    ] {
        assert!(text.contains(expected));
    }
    let events = ledger.events(target.id).await.unwrap();
    let completion = events
        .iter()
        .find(|event| event.event_type == "IssueCompleted")
        .unwrap();
    assert_eq!(
        completion.metadata.get("summary").unwrap(),
        "Finished task\nUnicode separator: \u{2028}"
    );
    fixture.assert_reaped().await;
}

#[tokio::test]
async fn lease_loss_aborts_pi_and_never_applies_a_result() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone())
        .with_lease_duration(std::time::Duration::from_millis(900))
        .unwrap();
    let human = ledger.ensure_peer("human", PeerKind::Human).await.unwrap();
    let peer = ledger.ensure_peer("worker", PeerKind::Agent).await.unwrap();
    let issue = ledger
        .create_issue(human.id, NewIssue::task("Interrupted execution"))
        .await
        .unwrap();
    let fixture = Fixture::new("cancel");
    let worker = Worker::new(
        ledger.clone(),
        Agent { peer_id: peer.id },
        fixture.backend(),
    );
    let task = tokio::spawn(async move { worker.tick().await });
    fixture.wait_file("accepted").await;
    let attempt = ledger
        .get_issue(issue.id)
        .await
        .unwrap()
        .current_attempt_id
        .unwrap();
    sqlx::query("UPDATE execution_attempts SET lease_expires_at=clock_timestamp()-interval '1 second' WHERE id=$1")
        .bind(attempt).execute(&database.pool).await.unwrap();
    let result = task.await.unwrap();
    assert!(matches!(result, Err(ledger::Error::ExecutionLost(id)) if id == attempt));
    fixture.wait_file("aborted").await;
    fixture.assert_reaped().await;
    assert!(
        !ledger
            .events(issue.id)
            .await
            .unwrap()
            .iter()
            .any(|event| event.event_type == "IssueCompleted")
    );
    assert_eq!(
        ledger.recover_expired_attempts().await.unwrap(),
        vec![issue.id]
    );
    assert_eq!(
        ledger.get_issue(issue.id).await.unwrap().status,
        IssueStatus::Ready
    );
}

#[tokio::test]
async fn different_models_keep_the_same_agent_identity_and_start_fresh_processes() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let human = ledger.ensure_peer("human", PeerKind::Human).await.unwrap();
    let peer = ledger.ensure_peer("worker", PeerKind::Agent).await.unwrap();
    let fixture = Fixture::new("normal");
    let mut attempts = Vec::new();
    for model in ["model-a", "model-b"] {
        let issue = ledger
            .create_issue(human.id, NewIssue::task("Independent execution"))
            .await
            .unwrap();
        let mut config = fixture.config();
        config.model = Some(model.into());
        let backend = PiBackend::new(config, LocalProcessEnvironment).unwrap();
        let worker = Worker::new(ledger.clone(), Agent { peer_id: peer.id }, backend);
        assert_eq!(
            worker.tick().await.unwrap().unwrap().status,
            IssueStatus::Completed
        );
        assert!(fixture.read("args").contains(model));
        let records = ledger.attempts(issue.id).await.unwrap();
        let attempt = records.first().unwrap();
        assert_eq!(attempt.peer_id, peer.id);
        attempts.push(attempt.id);
        fixture.assert_reaped().await;
    }
    assert_ne!(attempts.first(), attempts.last());
    assert_eq!(
        ledger
            .ensure_peer("worker", PeerKind::Agent)
            .await
            .unwrap()
            .id,
        peer.id
    );
}

#[tokio::test]
async fn backend_errors_fail_only_the_current_fenced_attempt() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let human = ledger.ensure_peer("human", PeerKind::Human).await.unwrap();
    let peer = ledger.ensure_peer("worker", PeerKind::Agent).await.unwrap();
    let issue = ledger
        .create_issue(human.id, NewIssue::task("Rejected prompt"))
        .await
        .unwrap();
    let fixture = Fixture::new("protocol_error");
    let worker = Worker::new(
        ledger.clone(),
        Agent { peer_id: peer.id },
        fixture.backend(),
    );
    assert_eq!(
        worker.tick().await.unwrap().unwrap().status,
        IssueStatus::Failed
    );
    let attempt = ledger.attempts(issue.id).await.unwrap();
    let attempt = attempt.first().unwrap();
    assert!(matches!(
        ledger
            .complete_attempt_with_summary(attempt.id, "stale summary")
            .await,
        Err(ledger::Error::ExecutionLost(_))
    ));
    fixture.assert_reaped().await;
}
