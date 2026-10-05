#![cfg(all(unix, feature = "rpc-fixture"))]
mod rpc_support;
use ledger::{AttemptId, Issue, Peer, PeerKind};
use rpc_support::Fixture;
use runtime::environment::LocalProcessEnvironment;
use runtime::{
    AgentBackend, BackendError, CancellationToken, ExecutionRequest, ExecutionResult, PiBackend,
};
use serde_json::Value;

#[tokio::test]
async fn launches_rpc_writes_jsonl_and_waits_for_settlement() {
    let fixture = Fixture::new("gated");
    let mut config = fixture.config();
    config.provider = Some("provider-a".into());
    config.model = Some("model-a".into());
    let backend = PiBackend::new(config, LocalProcessEnvironment).unwrap();
    let expected_peer = request().peer.id;
    let task =
        tokio::spawn(async move { backend.execute(request(), CancellationToken::new()).await });
    fixture.wait_file("accepted").await;
    assert!(
        !task.is_finished(),
        "acknowledgement and agent_end must not complete the attempt"
    );
    let args: Vec<String> = serde_json::from_str(&fixture.read("args")).unwrap();
    assert_eq!(
        args,
        [
            "--mode",
            "rpc",
            "--no-session",
            "--provider",
            "provider-a",
            "--model",
            "model-a"
        ]
    );
    let command: Value = serde_json::from_str(&fixture.read("prompt")).unwrap();
    assert_eq!(command.get("type").unwrap(), "prompt");
    assert_eq!(
        command.get("id").unwrap(),
        &request().attempt_id.to_string()
    );
    let message = command.get("message").unwrap().as_str().unwrap();
    assert!(message.contains(&expected_peer.to_string()));
    assert!(message.contains("Fix the build"));
    std::fs::write(fixture.workspace.join("release"), "yes").unwrap();
    let result = task.await.unwrap().unwrap();
    assert!(
        matches!(result, ExecutionResult::Completed { summary } if summary == "Finished task\nUnicode separator: \u{2028}")
    );
    fixture.assert_reaped().await;
}

#[tokio::test]
async fn fast_events_before_ack_and_retry_are_supported() {
    for mode in ["fast", "retry"] {
        let fixture = Fixture::new(mode);
        let result = fixture
            .backend()
            .execute(request(), CancellationToken::new())
            .await
            .unwrap();
        assert!(matches!(result, ExecutionResult::Completed { .. }));
        fixture.assert_reaped().await;
    }
}

#[tokio::test]
async fn malformed_oversized_and_rejected_records_are_errors() {
    for mode in ["malformed", "oversized", "protocol_error", "handled"] {
        let fixture = Fixture::new(mode);
        let result = fixture
            .backend()
            .execute(request(), CancellationToken::new())
            .await;
        assert!(
            matches!(result, Err(BackendError::Protocol(_))),
            "mode {mode}"
        );
        fixture.assert_reaped().await;
    }
}

#[tokio::test]
async fn unexpected_exit_and_spawn_failure_are_errors() {
    let fixture = Fixture::new("exit");
    let result = fixture
        .backend()
        .execute(request(), CancellationToken::new())
        .await;
    assert!(matches!(result, Err(BackendError::UnexpectedExit)));
    fixture.assert_reaped().await;
    let mut config = fixture.config();
    config.binary = fixture.workspace.join("absent-pi").into_os_string();
    let backend = PiBackend::new(config, LocalProcessEnvironment).unwrap();
    assert!(matches!(
        backend.execute(request(), CancellationToken::new()).await,
        Err(BackendError::Io(_))
    ));
}

#[tokio::test]
async fn explicit_failure_provider_failure_and_abort_have_distinct_results() {
    for mode in ["failure", "provider_error", "aborted_result"] {
        let fixture = Fixture::new(mode);
        let result = fixture
            .backend()
            .execute(request(), CancellationToken::new())
            .await
            .unwrap();
        if mode == "aborted_result" {
            assert!(matches!(result, ExecutionResult::Cancelled));
        } else {
            assert!(matches!(result, ExecutionResult::Failed { reason } if !reason.is_empty()));
        }
        fixture.assert_reaped().await;
    }
}

#[tokio::test]
async fn cancellation_aborts_and_reaps_cooperative_and_stubborn_children() {
    for mode in ["cancel", "stubborn"] {
        let fixture = Fixture::new(mode);
        let backend = fixture.backend();
        let cancellation = CancellationToken::new();
        let token = cancellation.clone();
        let task = tokio::spawn(async move { backend.execute(request(), token).await });
        fixture.wait_file("accepted").await;
        cancellation.cancel();
        assert!(matches!(
            task.await.unwrap().unwrap(),
            ExecutionResult::Cancelled
        ));
        fixture.wait_file("aborted").await;
        fixture.assert_reaped().await;
    }
}

#[tokio::test]
async fn dropping_the_execution_future_aborts_and_reaps_the_child() {
    let fixture = Fixture::new("stubborn");
    let backend = fixture.backend();
    let task =
        tokio::spawn(async move { backend.execute(request(), CancellationToken::new()).await });
    fixture.wait_file("accepted").await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    fixture.wait_file("aborted").await;
    fixture.assert_reaped().await;
}

#[tokio::test]
async fn continuously_drains_verbose_events_and_cancels() {
    let fixture = Fixture::new("flood");
    let backend = fixture.backend();
    let cancellation = CancellationToken::new();
    let token = cancellation.clone();
    let task = tokio::spawn(async move { backend.execute(request(), token).await });
    fixture.wait_file("flooded").await;
    cancellation.cancel();
    assert!(matches!(
        task.await.unwrap().unwrap(),
        ExecutionResult::Cancelled
    ));
    fixture.assert_reaped().await;
}

#[tokio::test]
async fn drains_large_stderr_and_stdout_during_orderly_shutdown() {
    for mode in ["stderr_flood", "post_settle_flood"] {
        let fixture = Fixture::new(mode);
        let mut config = fixture.config();
        config.shutdown_grace = std::time::Duration::from_secs(2);
        let backend = PiBackend::new(config, LocalProcessEnvironment).unwrap();
        let result = backend
            .execute(request(), CancellationToken::new())
            .await
            .expect(mode);
        assert!(matches!(result, ExecutionResult::Completed { .. }));
        if mode == "post_settle_flood" {
            fixture.wait_file("flooded").await;
        }
        fixture.assert_reaped().await;
    }
}

#[tokio::test]
async fn execution_deadline_aborts_and_reaps_the_child() {
    let fixture = Fixture::new("stubborn");
    let mut config = fixture.config();
    config.execution_timeout = std::time::Duration::from_secs(2);
    let backend = PiBackend::new(config, LocalProcessEnvironment).unwrap();
    assert!(matches!(
        backend.execute(request(), CancellationToken::new()).await,
        Err(BackendError::Timeout)
    ));
    fixture.wait_file("aborted").await;
    fixture.assert_reaped().await;
}

#[tokio::test]
async fn prompt_and_final_summary_are_bounded_on_utf8_boundaries() {
    let mut context = request();
    context.instructions = "🙂".repeat(20000);
    context.issue.description = "🙂".repeat(20000);
    context.children = vec![context.issue.clone(); 100];
    context.dependencies = context.children.clone();
    let prompt = runtime::pi::build_prompt(&context);
    assert!(prompt.len() <= runtime::pi::MAX_PROMPT_BYTES);
    assert!(prompt.contains("Switchboard owns durable coordination"));
    let fixture = Fixture::new("long");
    let result = fixture
        .backend()
        .execute(context, CancellationToken::new())
        .await
        .unwrap();
    assert!(
        matches!(result, ExecutionResult::Completed { summary } if summary.len() == runtime::pi::MAX_SUMMARY_BYTES)
    );
    fixture.assert_reaped().await;
}

#[tokio::test]
async fn validates_workspace_and_pre_cancel_does_not_spawn() {
    let fixture = Fixture::new("cancel");
    let mut config = fixture.config();
    config.workspace = fixture.workspace.join("absent");
    assert!(matches!(
        PiBackend::new(config, LocalProcessEnvironment)
            .unwrap()
            .execute(request(), CancellationToken::new())
            .await,
        Err(BackendError::Configuration(_))
    ));
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        fixture
            .backend()
            .execute(request(), cancellation)
            .await
            .unwrap(),
        ExecutionResult::Cancelled
    ));
    assert!(!fixture.workspace.join("pid").exists());
}

fn request() -> ExecutionRequest {
    let peer_id = "aaaaaaaa-aaaa-4aaa-aaaa-aaaaaaaaaaaa"
        .parse()
        .expect("valid fixture ID");
    let attempt_id: AttemptId = "bbbbbbbb-bbbb-4bbb-bbbb-bbbbbbbbbbbb"
        .parse()
        .expect("valid fixture ID");
    let issue: Issue = serde_json::from_value(serde_json::json!({
        "id":"cccccccc-cccc-4ccc-cccc-cccccccccccc", "title":"Fix the build", "description":"Run tests and fix failures",
        "kind":"Task", "status":"Running", "created_by":peer_id, "owner":peer_id, "current_attempt_id":attempt_id,
        "parent_id":null, "priority":0, "created_at":"2026-10-05T00:00:00Z", "updated_at":"2026-10-05T00:00:00Z"
    })).expect("valid fixture Issue");
    ExecutionRequest {
        attempt_id,
        peer: Peer {
            id: peer_id,
            name: "coding-agent".into(),
            kind: PeerKind::Agent,
        },
        instructions: "Verify changes".into(),
        issue,
        parent: None,
        children: vec![],
        dependencies: vec![],
        workspace: None,
    }
}
