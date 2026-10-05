use ledger::{Error, IssueKind, IssueStatus, Ledger, NewIssue, PeerKind};
use sqlx::PgPool;

#[sqlx::test(migrations = "../../migrations")]
async fn lifecycle_and_history(pool: PgPool) {
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
    let claimed = ledger.claim_issue(issue.id, agent.id).await.unwrap();
    assert_eq!(claimed.owner, Some(agent.id));
    assert_eq!(claimed.status, IssueStatus::Claimed);
    assert!(ledger.claim_next_issue(agent.id).await.unwrap().is_none());
    assert!(matches!(
        ledger.complete_issue(issue.id, agent.id).await,
        Err(Error::InvalidTransition(IssueStatus::Claimed))
    ));
    ledger.mark_running(issue.id, agent.id).await.unwrap();
    assert!(matches!(
        ledger.complete_issue(issue.id, human.id).await,
        Err(Error::IneligibleActor)
    ));
    ledger.complete_issue(issue.id, agent.id).await.unwrap();
    assert_eq!(
        ledger.get_issue(issue.id).await.unwrap().status,
        IssueStatus::Completed
    );
    assert!(matches!(
        ledger.mark_running(issue.id, agent.id).await,
        Err(Error::InvalidTransition(IssueStatus::Completed))
    ));
    let events = ledger.events(issue.id).await.unwrap();
    assert_eq!(
        events
            .iter()
            .map(|e| e.event_type.as_str())
            .collect::<Vec<_>>(),
        vec![
            "IssueCreated",
            "IssueClaimed",
            "IssueStarted",
            "IssueCompleted"
        ]
    );
    assert_eq!(events.last().unwrap().metadata["from"], "Running");
    assert!(matches!(
        sqlx::query("DELETE FROM issue_events WHERE issue_id=$1")
            .bind(issue.id)
            .execute(&pool)
            .await,
        Err(sqlx::Error::Database(_))
    ));
}

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_claims_are_unique(pool: PgPool) {
    let ledger = Ledger::from_pool(pool);
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
            ledger.claim_next_issue(agent.id).await.unwrap().unwrap().id
        });
    }
    let mut ids = std::collections::HashSet::new();
    while let Some(id) = tasks.join_next().await {
        assert!(ids.insert(id.unwrap()));
    }
    assert_eq!(ids.len(), 16);
    assert!(ledger.list_ready_issues().await.unwrap().is_empty());
}

#[sqlx::test(migrations = "../../migrations")]
async fn dependencies_children_and_humans(pool: PgPool) {
    let ledger = Ledger::from_pool(pool);
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

#[sqlx::test(migrations = "../../migrations")]
async fn backlog_failure_cancel_and_approval(pool: PgPool) {
    let ledger = Ledger::from_pool(pool);
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
    ledger.claim_issue(child.id, agent.id).await.unwrap();
    ledger
        .fail_issue(child.id, agent.id, "executor error")
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

#[sqlx::test(migrations = "../../migrations")]
async fn concurrent_cycle_and_completion_races(pool: PgPool) {
    let ledger = Ledger::from_pool(pool);
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
