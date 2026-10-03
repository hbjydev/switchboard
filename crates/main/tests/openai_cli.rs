#![expect(
    clippy::panic_in_result_fn,
    reason = "tests assert behavior while propagating setup errors"
)]
use axum::{Json, Router, routing::post};
use serde_json::{Value, json};
use std::process::Output;
use tokio::{net::TcpListener, process::Command, sync::mpsc};

type TestResult = Result<(), Box<dyn std::error::Error>>;
fn command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_switchboard"));
    // Inherit no user credentials, endpoint overrides, proxies, or model settings.
    command.env_clear();
    command
}
fn rejected(output: &Output, expected: &str) -> TestResult {
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = std::str::from_utf8(&output.stderr)?;
    assert!(stderr.contains(expected), "stderr: {stderr}");
    assert!(!stderr.contains("dummy-secret"));
    Ok(())
}

#[tokio::test]
async fn missing_credentials_and_invalid_configuration_fail_locally() -> TestResult {
    let output = command()
        .args(["openai", "--model", "test-model"])
        .output()
        .await?;
    rejected(&output, "OPENAI_API_KEY is required")?;
    let output = command()
        .env("OPENAI_API_KEY", "dummy-secret")
        .args(["openai", "--model", "test-model", "--timeout-seconds", "0"])
        .output()
        .await?;
    rejected(&output, "timeout must be greater than zero")?;
    let output = command()
        .env("OPENAI_API_KEY", "dummy-secret")
        .args([
            "openai",
            "--model",
            "test-model",
            "--base-url",
            "http://example.com/v1",
        ])
        .output()
        .await?;
    rejected(&output, "base URL must be HTTPS")?;
    let output = command()
        .env("OPENAI_API_KEY", "dummy-secret")
        .args(["openai", "--model", " "])
        .output()
        .await?;
    rejected(&output, "model must be nonblank")?;
    Ok(())
}

#[tokio::test]
async fn openai_command_uses_configuration_and_prints_attributed_transcript() -> TestResult {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let base_url = format!("http://{}/v1/", listener.local_addr()?);
    let (sender, mut requests) = mpsc::unbounded_channel();
    let app = Router::new().route("/v1/responses",post(move |Json(body): Json<Value>| {
        let sender = sender.clone();
        async move {
            let _ = sender.send(body);
            Json(json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"CLI reply"}]}]}))
        }
    }));
    let task = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let output = command()
        .env("OPENAI_API_KEY", "dummy-secret")
        .env("OPENAI_MODEL", "test-model")
        .env("OPENAI_BASE_URL", &base_url)
        .env("OPENAI_TIMEOUT_SECONDS", "2")
        .args([
            "openai",
            "--message",
            "CLI hello",
            "--instructions",
            "CLI instructions",
        ])
        .output()
        .await?;
    task.abort();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = std::str::from_utf8(&output.stdout)?;
    let lines = stdout.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 2);
    assert!(
        lines
            .first()
            .is_some_and(|line| line.starts_with("Human [") && line.ends_with("]: CLI hello"))
    );
    assert!(
        lines
            .last()
            .is_some_and(|line| line.starts_with("Agent [") && line.ends_with("]: CLI reply"))
    );
    let request = requests.try_recv()?;
    assert_eq!(request["model"], "test-model");
    assert_eq!(request["instructions"], "CLI instructions");
    assert!(!stdout.contains("dummy-secret"));
    Ok(())
}
