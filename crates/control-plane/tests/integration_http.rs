use std::{future::IntoFuture, time::Duration};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use control_plane::{
    Application,
    http::{ApiConfig, StatusResponse, router},
};
use ledger::{IssueId, IssueStatus, Ledger, PeerKind};
use runtime::{Agent, CancellationToken, FakeExecutor, Worker};
use serde_json::{Value, json};
use tower::ServiceExt;

#[path = "../../../tests/support/mod.rs"]
mod support;
use support::TestDatabase;

#[path = "../../../tests/support/oidc.rs"]
pub mod oidc;
use oidc::TestIssuer;

struct TestApi {
    router: Router,
    issuer: TestIssuer,
    token: String,
}

async fn api(ledger: Ledger) -> TestApi {
    let issuer = TestIssuer::start().await;
    let token = issuer.user_token("test-user", "switchboard:read switchboard:write");
    let router = router(
        Application::new(ledger),
        ApiConfig {
            auth: issuer.authenticator().await,
            status: StatusResponse {
                version: "test".into(),
                scheduler_enabled: false,
                scheduler_concurrency: 1,
                backend_kind: "disabled".into(),
            },
        },
    );
    TestApi {
        router,
        issuer,
        token,
    }
}

async fn request(
    api: &TestApi,
    method: &str,
    path: &str,
    body: Option<Value>,
    token: Option<&str>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    // None uses this test's signed user token; an empty override omits credentials.
    let token = token.unwrap_or(&api.token);
    if !token.is_empty() {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let request = if let Some(body) = body {
        request
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("valid JSON request")
    } else {
        request.body(Body::empty()).expect("valid empty request")
    };
    let response = api
        .router
        .clone()
        .oneshot(request)
        .await
        .expect("router response");
    assert!(response.headers().contains_key("x-request-id"));
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("bounded response body");
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).expect("JSON response")
    };
    (status, body)
}

async fn create(api: &TestApi, body: Value) -> Value {
    let (status, response) = request(api, "POST", "/api/v1/issues", Some(body), None).await;
    assert_eq!(status, StatusCode::CREATED);
    response
}

#[tokio::test]
async fn issue_intake_reads_and_controlled_lifecycle_over_http() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let api = api(ledger.clone()).await;
    let issue = create(
        &api,
        json!({"title":"HTTP work","backlog":true,"priority":3}),
    )
    .await;
    let id = issue["id"].as_str().unwrap();
    assert_eq!(issue["status"], "Backlog");
    assert_eq!(issue["kind"], "Task");
    let id_typed: IssueId = id.parse().unwrap();
    assert_eq!(ledger.get_issue(id_typed).await.unwrap().title, "HTTP work");
    let (status, shown) = request(&api, "GET", &format!("/api/v1/issues/{id}"), None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(shown, issue);
    let (_, list) = request(&api, "GET", "/api/v1/issues", None, None).await;
    assert_eq!(list.as_array().unwrap().len(), 1);
    let (_, ready) = request(&api, "GET", "/api/v1/issues?ready=true", None, None).await;
    assert!(ready.as_array().unwrap().is_empty());
    let (status, ready) = request(
        &api,
        "POST",
        &format!("/api/v1/issues/{id}/ready"),
        Some(json!({})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(ready["status"], "Ready");
    let (status, complete) = request(
        &api,
        "POST",
        &format!("/api/v1/issues/{id}/complete"),
        Some(json!({})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(complete["status"], "Completed");
    let (status, events) = request(
        &api,
        "GET",
        &format!("/api/v1/issues/{id}/events"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        events
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["event_type"] == "IssueCompleted")
    );
    let (status, attempts) = request(
        &api,
        "GET",
        &format!("/api/v1/issues/{id}/attempts"),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(attempts, json!([]));
    let cancelled = create(&api, json!({"title":"abandoned"})).await;
    let (status, cancelled) = request(
        &api,
        "POST",
        &format!(
            "/api/v1/issues/{}/cancel",
            cancelled["id"].as_str().unwrap()
        ),
        Some(json!({"reason":"no longer needed"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(cancelled["status"], "Cancelled");
}

#[tokio::test]
async fn human_answers_and_dependencies_preserve_ledger_semantics() {
    let database = TestDatabase::start().await.unwrap();
    let api = api(Ledger::from_pool(database.pool.clone())).await;
    let task = create(&api, json!({"title":"needs decision"})).await;
    let question = create(&api, json!({"title":"which option?","kind":"Question"})).await;
    assert_eq!(question["status"], "WaitingForHuman");
    let task_path = format!("/api/v1/issues/{}", task["id"].as_str().unwrap());
    let (status, _) = request(
        &api,
        "POST",
        &format!("{task_path}/dependencies"),
        Some(json!({"dependency_id":question["id"]})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, blocked) = request(&api, "GET", &task_path, None, None).await;
    assert_eq!(blocked["status"], "WaitingForHuman");
    let (status, answered) = request(
        &api,
        "POST",
        &format!("/api/v1/issues/{}/answer", question["id"].as_str().unwrap()),
        Some(json!({"answer":"option A"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(answered["status"], "Completed");
    let (_, unblocked) = request(&api, "GET", &task_path, None, None).await;
    assert_eq!(unblocked["status"], "Ready");
    let other = create(&api, json!({"title":"other task"})).await;
    let (status, _) = request(
        &api,
        "POST",
        &format!("{task_path}/dependencies"),
        Some(json!({"dependency_id":other["id"]})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (_, blocked) = request(&api, "GET", &task_path, None, None).await;
    assert_eq!(blocked["status"], "Blocked");
    let (status, error) = request(
        &api,
        "POST",
        &format!(
            "/api/v1/issues/{}/dependencies",
            other["id"].as_str().unwrap()
        ),
        Some(json!({"dependency_id":task["id"]})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "dependency_cycle");
    let approval = create(&api, json!({"title":"approval","kind":"Approval"})).await;
    let (status, _) = request(
        &api,
        "POST",
        &format!("/api/v1/issues/{}/answer", approval["id"].as_str().unwrap()),
        Some(json!({"answer":"approved"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn errors_are_structured_and_do_not_expose_internal_details() {
    let database = TestDatabase::start().await.unwrap();
    let api = api(Ledger::from_pool(database.pool.clone())).await;
    let issue = create(&api, json!({"title":"backlog","backlog":true})).await;
    let (status, error) = request(
        &api,
        "POST",
        &format!("/api/v1/issues/{}/complete", issue["id"].as_str().unwrap()),
        Some(json!({})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "invalid_transition");
    assert!(error["error"]["message"].is_string());
    for suffix in ["", "/events", "/attempts", "/ready", "/complete"] {
        let method = if matches!(suffix, "/ready" | "/complete") {
            "POST"
        } else {
            "GET"
        };
        let (status, error) = request(
            &api,
            method,
            &format!("/api/v1/issues/{}{suffix}", uuid::Uuid::new_v4()),
            (method == "POST").then(|| json!({})),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(error["error"]["code"], "issue_not_found");
    }
    for (method, path, body) in [
        ("GET", "/api/v1/issues/not-a-uuid", None),
        ("GET", "/api/v1/issues?ready=bogus", None),
        (
            "POST",
            "/api/v1/issues",
            Some(json!({"title":"x","status":"Completed"})),
        ),
        ("POST", "/api/v1/issues", Some(json!({"title":3}))),
    ] {
        let (status, error) = request(&api, method, path, body, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["error"]["code"], "invalid_request");
    }
    let (status, error) = request(
        &api,
        "POST",
        "/api/v1/issues",
        Some(json!({"title":" "})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(error["error"]["code"], "empty_field");
    let (status, error) = request(
        &api,
        "POST",
        "/api/v1/issues",
        Some(json!({"title":"large","description":"x".repeat(65536)})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(error["error"]["code"], "payload_too_large");
    let (status, error) = request(&api, "DELETE", "/api/v1/issues", None, None).await;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(error["error"]["code"], "method_not_allowed");
}

#[tokio::test]
async fn oidc_auth_is_mandatory_and_health_remains_public() {
    let database = TestDatabase::start().await.unwrap();
    let api = api(Ledger::from_pool(database.pool.clone())).await;
    for path in ["/healthz", "/readyz"] {
        assert_eq!(
            request(&api, "GET", path, None, Some("")).await.0,
            StatusCode::OK
        );
    }
    for path in ["/api/v1/issues", "/api/v1/status", "/api/v1/unknown"] {
        for token in [Some(""), Some("wrong"), Some("test-token")] {
            let (status, error) = request(&api, "GET", path, None, token).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            assert_eq!(error["error"]["code"], "unauthorized");
            assert!(!error.to_string().contains("test-token"));
        }
    }
    assert_eq!(
        request(
            &api,
            "POST",
            "/api/v1/issues",
            Some(json!({"title":"unauthenticated"})),
            Some("")
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(
            &api,
            "POST",
            "/api/v1/issues",
            Some(json!({"title":"authenticated"})),
            None
        )
        .await
        .0,
        StatusCode::CREATED
    );
    let (status, info) = request(&api, "GET", "/api/v1/status", None, None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["scheduler_enabled"], false);
    assert!(!info.to_string().contains("test-token"));
}

#[tokio::test]
async fn readiness_checks_database_and_schema_without_mutating() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    ledger.check_ready().await.unwrap();
    let api = api(ledger.clone()).await;
    assert_eq!(
        request(&api, "GET", "/readyz", None, None).await.0,
        StatusCode::OK
    );
    sqlx::query("UPDATE _sqlx_migrations SET checksum='\\x00'::bytea WHERE version=2")
        .execute(&database.pool)
        .await
        .unwrap();
    assert!(matches!(
        ledger.check_ready().await,
        Err(ledger::Error::SchemaNotReady)
    ));
    assert_eq!(
        request(&api, "GET", "/readyz", None, None).await.0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    let unchanged: Vec<i64> =
        sqlx::query_scalar("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&database.pool)
            .await
            .unwrap();
    assert_eq!(unchanged, [1, 2, 3]);
    database.pool.close().await;
    assert_eq!(
        request(&api, "GET", "/healthz", None, None).await.0,
        StatusCode::OK
    );
    let (status, error) = request(&api, "GET", "/readyz", None, None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error["error"]["code"], "ledger_unavailable");
    let (status, error) = request(&api, "GET", "/api/v1/issues", None, None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(!error.to_string().contains("SELECT"));
    assert!(!error.to_string().contains("postgres"));
}

#[tokio::test]
async fn tcp_server_and_scheduler_execute_http_intake() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let peer = ledger.ensure_peer("worker", PeerKind::Agent).await.unwrap();
    let shutdown = CancellationToken::new();
    let scheduler = tokio::spawn(control_plane::scheduler::run(
        Worker::new(ledger.clone(), Agent { peer_id: peer.id }, FakeExecutor),
        control_plane::scheduler::SchedulerConfig {
            interval: Duration::from_millis(20),
            ..Default::default()
        },
        shutdown.clone(),
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let api = api(ledger.clone()).await;
    let server = tokio::spawn(
        axum::serve(listener, api.router.clone())
            .with_graceful_shutdown(shutdown.clone().cancelled_owned())
            .into_future(),
    );
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .get(format!("http://{address}/healthz"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let response = client
        .post(format!("http://{address}/api/v1/issues"))
        .bearer_auth(&api.token)
        .json(&json!({"title":"automatic"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let issue: Value = response.json().await.unwrap();
    let id: IssueId = issue["id"].as_str().unwrap().parse().unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while ledger.get_issue(id).await.unwrap().status != IssueStatus::Completed {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let attempts: Value = client
        .get(format!("http://{address}/api/v1/issues/{id}/attempts"))
        .bearer_auth(&api.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(attempts.as_array().unwrap().len(), 1);
    assert_eq!(attempts[0]["attempt"]["state"], "Completed");
    assert_eq!(attempts[0]["current"], false);
    assert_eq!(attempts[0]["peer"]["name"], "worker");
    shutdown.cancel();
    scheduler.await.unwrap().unwrap();
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn scopes_and_principal_kind_control_api_access() {
    let database = TestDatabase::start().await.unwrap();
    let api = api(Ledger::from_pool(database.pool.clone())).await;
    let read = api.issuer.user_token("reader", "switchboard:read");
    let workload = api
        .issuer
        .workload_token("remote-environment", "switchboard:read switchboard:write");
    let no_scope = api.issuer.user_token("no-permissions", "openid profile");
    assert_eq!(
        request(&api, "GET", "/api/v1/issues", None, Some(&read))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&api, "GET", "/api/v1/status", None, Some(&workload))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(&api, "GET", "/api/v1/issues", None, Some(&no_scope))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let (status, error) = request(
        &api,
        "POST",
        "/api/v1/issues",
        Some(json!({"title":"read-only write"})),
        Some(&read),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error["error"]["code"], "insufficient_scope");
    for path in [
        "/api/v1/issues",
        "/api/v1/issues/00000000-0000-0000-0000-000000000000/answer",
        "/api/v1/issues/00000000-0000-0000-0000-000000000000/cancel",
    ] {
        let (status, error) = request(&api, "POST", path, Some(json!({})), Some(&workload)).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(error["error"]["code"], "workload_not_authorized");
    }
    let (_, identity) = request(&api, "GET", "/api/v1/me", None, Some(&workload)).await;
    assert_eq!(identity["kind"], "Workload");
    assert_eq!(identity["client_id"], "remote-environment");
    let peer: ledger::Peer = sqlx::query_as("SELECT * FROM peers WHERE id=$1")
        .bind(
            identity["peer_id"]
                .as_str()
                .unwrap()
                .parse::<uuid::Uuid>()
                .unwrap(),
        )
        .fetch_one(&database.pool)
        .await
        .unwrap();
    assert_eq!(peer.kind, PeerKind::Agent);
}

#[tokio::test]
async fn verified_identity_controls_attribution_and_client_cannot_impersonate() {
    let database = TestDatabase::start().await.unwrap();
    let ledger = Ledger::from_pool(database.pool.clone());
    let api = api(ledger.clone()).await;
    let (status, error) = request(
        &api,
        "POST",
        "/api/v1/issues",
        Some(json!({"title":"spoofed","actor":"operator"})),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(error["error"]["code"], "invalid_request");
    let issue = create(&api, json!({"title":"real identity"})).await;
    let (_, me) = request(&api, "GET", "/api/v1/me", None, None).await;
    assert_eq!(issue["created_by"], me["peer_id"]);
    assert_eq!(me["subject"], "test-user");
    let mut updated = api
        .issuer
        .user_claims("test-user", "switchboard:read switchboard:write");
    updated["preferred_username"] = json!("administrator");
    updated["name"] = json!("Some other display name");
    let token = api.issuer.sign(&updated);
    let (_, same) = request(&api, "GET", "/api/v1/me", None, Some(&token)).await;
    assert_eq!(same["peer_id"], me["peer_id"]);
    let id = issue["id"].as_str().unwrap();
    let (status, _) = request(
        &api,
        "POST",
        &format!("/api/v1/issues/{id}/complete"),
        Some(json!({})),
        Some(&token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, events) = request(
        &api,
        "GET",
        &format!("/api/v1/issues/{id}/events"),
        None,
        None,
    )
    .await;
    assert!(
        events
            .as_array()
            .unwrap()
            .iter()
            .all(|event| event["actor"] == me["peer_id"])
    );
    let conflicting = api.issuer.workload_token("test-user", "switchboard:read");
    let (status, error) = request(&api, "GET", "/api/v1/me", None, Some(&conflicting)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(error["error"]["code"], "identity_kind_conflict");
    let refreshed_api = router(
        Application::new(ledger),
        ApiConfig {
            auth: api.issuer.authenticator().await,
            status: StatusResponse {
                version: "test".into(),
                scheduler_enabled: false,
                scheduler_concurrency: 1,
                backend_kind: "disabled".into(),
            },
        },
    );
    let restarted = TestApi {
        router: refreshed_api,
        issuer: api.issuer,
        token: api.token,
    };
    let (_, after_restart) = request(&restarted, "GET", "/api/v1/me", None, None).await;
    assert_eq!(after_restart["peer_id"], me["peer_id"]);
}
