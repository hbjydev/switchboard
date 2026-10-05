use std::time::Duration;

use control_plane::{
    Application,
    http::{ApiConfig, StatusResponse},
};
use ledger::{IssueStatus, Ledger};
use serde_json::Value;
use tokio::process::Command;

#[path = "support/oidc.rs"]
pub mod oidc;
mod support;
use oidc::TestIssuer;
use support::TestDatabase;

struct TestServer {
    url: String,
    access_token: String,
    task: tokio::task::JoinHandle<()>,
}

impl TestServer {
    async fn start(ledger: Ledger, issuer: &TestIssuer) -> Self {
        let router = control_plane::http::router(
            Application::new(ledger),
            ApiConfig {
                auth: issuer.authenticator().await,
                status: StatusResponse {
                    version: "test".to_owned(),
                    scheduler_enabled: false,
                    scheduler_concurrency: 1,
                    backend_kind: "disabled".to_owned(),
                },
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture operation failed");
        let url = format!(
            "http://{}",
            listener.local_addr().expect("fixture operation failed")
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("fixture operation failed");
        });
        Self {
            url,
            access_token: issuer.user_token("fixture-user", "switchboard:read switchboard:write"),
            task,
        }
    }

    async fn issue(&self, args: &[&str]) -> Value {
        let output = tokio::time::timeout(
            Duration::from_secs(15),
            Command::new(env!("CARGO_BIN_EXE_switchboard"))
                .env_remove("DATABASE_URL")
                .env_remove("SWITCHBOARD_LEASE_SECONDS")
                .env("SWITCHBOARD_URL", &self.url)
                .env("SWITCHBOARD_ACCESS_TOKEN", &self.access_token)
                .arg("issue")
                .args(args)
                .output(),
        )
        .await
        .expect("fixture operation failed")
        .expect("fixture operation failed");
        assert!(
            output.status.success(),
            "CLI failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("fixture operation failed")
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn remote_cli_uses_api_for_core_issue_operations_without_database_credentials() {
    let database = TestDatabase::start()
        .await
        .expect("fixture operation failed");
    let ledger = Ledger::from_pool(database.pool.clone());
    let issuer = TestIssuer::start().await;
    let server = TestServer::start(ledger.clone(), &issuer).await;
    let prerequisite = server.issue(&["create", "--title", "Prerequisite"]).await;
    let prerequisite_id = prerequisite["id"]
        .as_str()
        .expect("fixture operation failed");
    let task = server
        .issue(&["create", "--title", "Remote task", "--backlog"])
        .await;
    let id = task["id"].as_str().expect("fixture operation failed");
    let _dependency = server.issue(&["depend", id, prerequisite_id]).await;
    let _ready = server.issue(&["ready", id]).await;
    let shown = server.issue(&["show", id]).await;
    assert_eq!(shown["status"], "Blocked");
    let _completed = server.issue(&["complete", prerequisite_id]).await;
    let shown = server.issue(&["show", id]).await;
    assert_eq!(shown["status"], "Ready");
    let listed = server.issue(&["list", "--ready"]).await;
    assert!(
        listed
            .as_array()
            .expect("fixture operation failed")
            .iter()
            .any(|issue| issue["id"] == id)
    );
    let _completed = server.issue(&["complete", id]).await;
    let events = server.issue(&["events", id]).await;
    assert!(
        !events
            .as_array()
            .expect("fixture operation failed")
            .is_empty()
    );
    let attempts = server.issue(&["attempts", id]).await;
    assert!(
        attempts
            .as_array()
            .expect("fixture operation failed")
            .is_empty()
    );
    assert_eq!(
        ledger
            .get_issue(id.parse().expect("fixture operation failed"))
            .await
            .expect("fixture operation failed")
            .status,
        IssueStatus::Completed
    );
    let question = server
        .issue(&["create", "--title", "Decision?", "--kind", "question"])
        .await;
    let question_id = question["id"].as_str().expect("fixture operation failed");
    let _answer = server
        .issue(&["answer", question_id, "--answer", "Use option A"])
        .await;
    assert_eq!(
        server.issue(&["show", question_id]).await["status"],
        "Completed"
    );
    let abandoned = server.issue(&["create", "--title", "Abandoned"]).await;
    let abandoned_id = abandoned["id"].as_str().expect("fixture operation failed");
    let _cancelled = server
        .issue(&["cancel", abandoned_id, "--reason", "No longer needed"])
        .await;
    assert_eq!(
        server.issue(&["show", abandoned_id]).await["status"],
        "Cancelled"
    );
}

struct ProcessServer {
    url: String,
    child: tokio::process::Child,
}

impl ProcessServer {
    async fn start(database: &TestDatabase, issuer: &TestIssuer, arguments: &[&str]) -> Self {
        let reservation =
            std::net::TcpListener::bind("127.0.0.1:0").expect("fixture operation failed");
        let address = reservation.local_addr().expect("fixture operation failed");
        drop(reservation);
        let options = database.pool.connect_options();
        let database_url = format!(
            "postgres://postgres:postgres@{}:{}/postgres",
            options.get_host(),
            options.get_port()
        );
        let child = Command::new(env!("CARGO_BIN_EXE_switchboard"))
            .env_remove("SWITCHBOARD_URL")
            .env_remove("SWITCHBOARD_WORKSPACE")
            .env_remove("SWITCHBOARD_PI_PROVIDER")
            .env_remove("SWITCHBOARD_PI_MODEL")
            .env("DATABASE_URL", database_url)
            .env("SWITCHBOARD_OIDC_ISSUER", &issuer.issuer)
            .env("SWITCHBOARD_OIDC_USER_AUDIENCE", oidc::USER_AUDIENCE)
            .env(
                "SWITCHBOARD_OIDC_WORKLOAD_AUDIENCE",
                oidc::WORKLOAD_AUDIENCE,
            )
            .env("SWITCHBOARD_OIDC_ALLOW_LOOPBACK_HTTP", "true")
            .env("SWITCHBOARD_AGENT_NAME", "fixture-agent")
            .env("SWITCHBOARD_LEASE_SECONDS", "30")
            .env("SWITCHBOARD_WORKER_CONCURRENCY", "2")
            .env("SWITCHBOARD_SCHEDULER_INTERVAL_MS", "20")
            .env("SWITCHBOARD_SHUTDOWN_GRACE_SECONDS", "2")
            .arg("serve")
            .arg("--listen-address")
            .arg(address.to_string())
            .args(arguments)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("fixture operation failed");
        let server = Self {
            url: format!("http://{address}"),
            child,
        };
        let client = reqwest::Client::new();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(response) = client.get(format!("{}/healthz", server.url)).send().await
                    && response.status().is_success()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("serve did not become healthy");
        server
    }

    #[cfg(unix)]
    async fn terminate(mut self) {
        let status = Command::new("kill")
            .arg("-TERM")
            .arg(
                self.child
                    .id()
                    .expect("fixture operation failed")
                    .to_string(),
            )
            .status()
            .await
            .expect("fixture operation failed");
        assert!(status.success());
        let status = tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .expect("SIGTERM did not shut down server")
            .expect("fixture operation failed");
        assert!(status.success(), "server failed during shutdown");
    }
}

#[tokio::test]
async fn serve_api_only_needs_no_pi_configuration_and_handles_sigterm() {
    let database = TestDatabase::start()
        .await
        .expect("fixture operation failed");
    let issuer = TestIssuer::start().await;
    let server = ProcessServer::start(
        &database,
        &issuer,
        &["--scheduler=false", "--backend=pi-local"],
    )
    .await;
    let client = reqwest::Client::new();
    assert!(
        client
            .get(format!("{}/readyz", server.url))
            .send()
            .await
            .expect("fixture operation failed")
            .status()
            .is_success()
    );
    let status: Value = client
        .get(format!("{}/api/v1/status", server.url))
        .bearer_auth(issuer.user_token("fixture-user", "switchboard:read switchboard:write"))
        .send()
        .await
        .expect("fixture operation failed")
        .json()
        .await
        .expect("fixture operation failed");
    assert_eq!(status["scheduler_enabled"], false);
    #[cfg(unix)]
    server.terminate().await;
}

#[tokio::test]
async fn serve_scheduler_completes_http_intake_without_a_separate_worker() {
    let database = TestDatabase::start()
        .await
        .expect("fixture operation failed");
    let issuer = TestIssuer::start().await;
    let server =
        ProcessServer::start(&database, &issuer, &["--scheduler=true", "--backend=fake"]).await;
    let client = reqwest::Client::new();
    let issue: Value = client
        .post(format!("{}/api/v1/issues", server.url))
        .bearer_auth(issuer.user_token("fixture-user", "switchboard:read switchboard:write"))
        .json(&serde_json::json!({ "title": "Production intake", "kind": "Task" }))
        .send()
        .await
        .expect("fixture operation failed")
        .json()
        .await
        .expect("fixture operation failed");
    let id: ledger::IssueId = issue["id"]
        .as_str()
        .expect("fixture operation failed")
        .parse()
        .expect("fixture operation failed");
    let ledger = Ledger::from_pool(database.pool.clone());
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if ledger
                .get_issue(id)
                .await
                .expect("fixture operation failed")
                .status
                == IssueStatus::Completed
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("serve scheduler did not complete work");
    assert_eq!(
        ledger
            .attempts(id)
            .await
            .expect("fixture operation failed")
            .len(),
        1
    );
    #[cfg(unix)]
    server.terminate().await;
}

#[tokio::test]
async fn serve_fails_without_migrating_an_uninitialized_schema() {
    let database = TestDatabase::start()
        .await
        .expect("fixture operation failed");
    sqlx::query("DROP SCHEMA public CASCADE")
        .execute(&database.pool)
        .await
        .expect("fixture operation failed");
    sqlx::query("CREATE SCHEMA public")
        .execute(&database.pool)
        .await
        .expect("fixture operation failed");
    let options = database.pool.connect_options();
    let database_url = format!(
        "postgres://postgres:postgres@{}:{}/postgres",
        options.get_host(),
        options.get_port()
    );
    let issuer = TestIssuer::start().await;
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new(env!("CARGO_BIN_EXE_switchboard"))
            .env("DATABASE_URL", database_url)
            .env_remove("SWITCHBOARD_URL")
            .env("SWITCHBOARD_OIDC_ISSUER", &issuer.issuer)
            .env("SWITCHBOARD_OIDC_USER_AUDIENCE", oidc::USER_AUDIENCE)
            .env("SWITCHBOARD_OIDC_ALLOW_LOOPBACK_HTTP", "true")
            .env("SWITCHBOARD_LEASE_SECONDS", "30")
            .arg("serve")
            .arg("--scheduler=false")
            .arg("--listen-address=127.0.0.1:0")
            .output(),
    )
    .await
    .expect("fixture operation failed")
    .expect("fixture operation failed");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("run switchboard migrate"));
    let has_issues: bool = sqlx::query_scalar("SELECT to_regclass('public.issues') IS NOT NULL")
        .fetch_one(&database.pool)
        .await
        .expect("fixture operation failed");
    assert!(!has_issues, "serve must not silently migrate");
}

struct CredentialFile {
    directory: std::path::PathBuf,
    path: std::path::PathBuf,
}

impl CredentialFile {
    fn new() -> Self {
        let directory =
            std::env::temp_dir().join(format!("switchboard-login-{}", uuid::Uuid::new_v4()));
        Self {
            path: directory.join("credentials.json"),
            directory,
        }
    }

    fn read(&self) -> Value {
        serde_json::from_slice(&std::fs::read(&self.path).expect("credentials saved"))
            .expect("JSON credentials")
    }
}

impl Drop for CredentialFile {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.directory);
    }
}

fn auth_process(issuer: &TestIssuer, credentials: &CredentialFile) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_switchboard"));
    command
        .env_remove("DATABASE_URL")
        .env_remove("SWITCHBOARD_URL")
        .env_remove("SWITCHBOARD_ACCESS_TOKEN")
        .env_remove("SWITCHBOARD_OAUTH_SCOPES")
        .env_remove("SWITCHBOARD_OAUTH_RESOURCE")
        .env("SWITCHBOARD_OIDC_ISSUER", &issuer.issuer)
        .env("SWITCHBOARD_OIDC_CLIENT_ID", oidc::CLI_ID)
        .env("SWITCHBOARD_OIDC_USER_AUDIENCE", oidc::USER_AUDIENCE)
        .env(
            "SWITCHBOARD_OIDC_WORKLOAD_AUDIENCE",
            oidc::WORKLOAD_AUDIENCE,
        )
        .env("SWITCHBOARD_OIDC_ALLOW_LOOPBACK_HTTP", "true")
        .env("SWITCHBOARD_CREDENTIALS_FILE", &credentials.path)
        .kill_on_drop(true);
    command
}

async fn browser_login(
    issuer: &TestIssuer,
    credentials: &CredentialFile,
    tamper_nonce: bool,
) -> std::process::Output {
    use tokio::io::AsyncBufReadExt;
    let mut child = auth_process(issuer, credentials)
        .args([
            "auth",
            "login",
            "--no-browser",
            "--redirect-port=0",
            "--timeout-seconds=5",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("login child");
    let stderr = child.stderr.take().expect("login stderr");
    let mut lines = tokio::io::BufReader::new(stderr).lines();
    let authorization = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let line = lines
                .next_line()
                .await
                .expect("login output line")
                .expect("authorization URL output");
            if line.starts_with("http://") {
                break line;
            }
        }
    })
    .await
    .expect("authorization URL timeout");
    let mut authorization = reqwest::Url::parse(&authorization).expect("authorization URL");
    let pairs: std::collections::HashMap<_, _> = authorization.query_pairs().into_owned().collect();
    assert_eq!(
        pairs.get("code_challenge_method").map(String::as_str),
        Some("S256")
    );
    assert!(!pairs.contains_key("client_secret"));
    let mut wrong_callback =
        reqwest::Url::parse(pairs.get("redirect_uri").expect("loopback redirect"))
            .expect("callback URL");
    wrong_callback
        .query_pairs_mut()
        .append_pair("state", "forged")
        .append_pair("code", "forged");
    assert_eq!(
        reqwest::get(wrong_callback)
            .await
            .expect("forged callback request")
            .status(),
        reqwest::StatusCode::BAD_REQUEST
    );
    if tamper_nonce {
        let modified: Vec<_> = authorization
            .query_pairs()
            .into_owned()
            .map(|(key, value)| {
                let value = if key == "nonce" {
                    "substituted-nonce".to_owned()
                } else {
                    value
                };
                (key, value)
            })
            .collect();
        authorization.set_query(None);
        authorization.query_pairs_mut().extend_pairs(modified);
    }
    assert!(
        reqwest::get(authorization)
            .await
            .expect("mock browser authorization")
            .status()
            .is_success()
    );
    tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .expect("login completion timeout")
        .expect("login output")
}

#[tokio::test]
async fn browser_pkce_login_caches_private_credentials_refreshes_and_logs_out() {
    let issuer = TestIssuer::start().await;
    issuer.omit_nonce_on_refresh();
    let credentials = CredentialFile::new();
    let output = browser_login(&issuer, &credentials, false).await;
    assert!(
        output.status.success(),
        "login failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let original = credentials.read();
    assert_eq!(original["subject"], "fixture-user");
    assert_eq!(original["kind"], "user");
    assert!(
        original["nonce"]
            .as_str()
            .is_some_and(|nonce| nonce.len() >= 43)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&credentials.path)
                .expect("credential metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
    let database = TestDatabase::start().await.expect("isolated database");
    let server = TestServer::start(Ledger::from_pool(database.pool.clone()), &issuer).await;
    let mut expired = original.clone();
    expired["expires_at"] = serde_json::json!(0);
    std::fs::write(
        &credentials.path,
        serde_json::to_vec(&expired).expect("expired cache JSON"),
    )
    .expect("force expiry");
    let output = auth_process(&issuer, &credentials)
        .env("SWITCHBOARD_URL", &server.url)
        .args(["issue", "create", "--title", "Signed in through browser"])
        .output()
        .await
        .expect("authenticated CLI request");
    assert!(
        output.status.success(),
        "cached request failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let refreshed = credentials.read();
    assert_ne!(refreshed["refresh_token"], original["refresh_token"]);
    assert!(
        refreshed["expires_at"]
            .as_u64()
            .is_some_and(|expiry| expiry > 0)
    );
    #[cfg(unix)]
    assert_private_cache_and_token_override(&issuer, &credentials, &server).await;
    let output = auth_process(&issuer, &credentials)
        .args(["auth", "logout"])
        .output()
        .await
        .expect("logout");
    assert!(output.status.success());
    assert!(!credentials.path.exists());
}

#[tokio::test]
async fn browser_login_rejects_nonce_substitution_without_saving_credentials() {
    let issuer = TestIssuer::start().await;
    let credentials = CredentialFile::new();
    let output = browser_login(&issuer, &credentials, true).await;
    assert!(!output.status.success());
    assert!(!credentials.path.exists());
}

#[tokio::test]
async fn client_credentials_stores_workload_token_without_secret_and_calls_api() {
    let issuer = TestIssuer::start().await;
    issuer.omit_expires_in();
    let credentials = CredentialFile::new();
    let output = auth_process(&issuer, &credentials)
        .env("SWITCHBOARD_OAUTH_CLIENT_ID", "fixture-workload")
        .env("SWITCHBOARD_OAUTH_CLIENT_SECRET", "fixture-secret")
        .args(["auth", "client-credentials"])
        .output()
        .await
        .expect("workload token command");
    assert!(
        output.status.success(),
        "workload auth failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let saved = credentials.read();
    assert_eq!(saved["kind"], "workload");
    assert_eq!(saved["subject"], "fixture-workload");
    assert!(
        !std::fs::read_to_string(&credentials.path)
            .expect("credential file")
            .contains("fixture-secret")
    );
    assert!(
        !String::from_utf8_lossy(&output.stdout)
            .contains(saved["access_token"].as_str().expect("access token"))
    );
    let database = TestDatabase::start().await.expect("isolated database");
    let server = TestServer::start(Ledger::from_pool(database.pool.clone()), &issuer).await;
    let output = auth_process(&issuer, &credentials)
        .env("SWITCHBOARD_URL", &server.url)
        .args(["issue", "list"])
        .output()
        .await
        .expect("workload API request");
    assert!(output.status.success());
    let output = auth_process(&issuer, &credentials)
        .env("SWITCHBOARD_URL", &server.url)
        .args([
            "issue",
            "create",
            "--title",
            "Forbidden for read-only client",
        ])
        .output()
        .await
        .expect("read-only workload mutation");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("403"));
}

#[cfg(unix)]
async fn assert_private_cache_and_token_override(
    issuer: &TestIssuer,
    credentials: &CredentialFile,
    server: &TestServer,
) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&credentials.path, std::fs::Permissions::from_mode(0o644))
        .expect("insecure cache permissions");
    let rejected = auth_process(issuer, credentials)
        .env("SWITCHBOARD_URL", &server.url)
        .args(["issue", "list"])
        .output()
        .await
        .expect("insecure cache check");
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("private"));
    assert!(!String::from_utf8_lossy(&rejected.stderr).contains(&server.access_token));
    let overridden = auth_process(issuer, credentials)
        .env("SWITCHBOARD_URL", &server.url)
        .env("SWITCHBOARD_ACCESS_TOKEN", &server.access_token)
        .args(["issue", "list"])
        .output()
        .await
        .expect("explicit token override");
    assert!(overridden.status.success());
    std::fs::set_permissions(&credentials.path, std::fs::Permissions::from_mode(0o600))
        .expect("restore private permissions");
    let moved = credentials.directory.join("original.json");
    std::fs::rename(&credentials.path, &moved).expect("move original credentials");
    std::os::unix::fs::symlink(&moved, &credentials.path).expect("symlink credentials");
    let rejected = auth_process(issuer, credentials)
        .env("SWITCHBOARD_URL", &server.url)
        .args(["issue", "list"])
        .output()
        .await
        .expect("symlink cache check");
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("regular file"));
    std::fs::remove_file(&credentials.path).expect("remove symlink");
    std::fs::rename(moved, &credentials.path).expect("restore original credentials");
}
