use anyhow::{Context, Result};
use reqwest::{Client, Method, Url};
use serde_json::{Value, json};

use crate::IssueCommand;

/// Administrative client: no database credentials or domain mutation implementation.
pub struct ApiClient {
    client: Client,
    base: Url,
    token: String,
}

impl ApiClient {
    pub fn new(base: &str, token: String) -> Result<Self> {
        let base = Url::parse(base).context("SWITCHBOARD_URL must be a valid HTTP(S) URL")?;
        anyhow::ensure!(
            matches!(base.scheme(), "http" | "https") && base.host_str().is_some(),
            "SWITCHBOARD_URL must be a valid HTTP(S) URL"
        );
        let loopback = base
            .host_str()
            .and_then(|host| {
                host.trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .ok()
            })
            .is_some_and(|address| address.is_loopback());
        anyhow::ensure!(
            base.scheme() == "https" || loopback,
            "SWITCHBOARD_URL must use HTTPS except for literal loopback addresses"
        );
        anyhow::ensure!(
            base.username().is_empty() && base.password().is_none(),
            "put the access token in SWITCHBOARD_ACCESS_TOKEN, not SWITCHBOARD_URL"
        );
        Ok(Self {
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(30))
                .build()?,
            base,
            token,
        })
    }

    async fn request(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let url = self.base.join(path)?;
        let mut request = self.client.request(method, url);
        request = request.bearer_auth(&self.token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        // Deliberately avoid displaying reqwest's error URL: configuration can contain secrets.
        let response = request
            .send()
            .await
            .map_err(|_error| anyhow::anyhow!("could not reach the Switchboard API"))?;
        let status = response.status();
        if status == reqwest::StatusCode::NO_CONTENT {
            return Ok(Value::Null);
        }
        let value: Value = response
            .json()
            .await
            .context("the Switchboard API returned invalid JSON")?;
        if !status.is_success() {
            let code = value
                .pointer("/error/code")
                .and_then(Value::as_str)
                .unwrap_or("api_error");
            let message = value
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or("request failed");
            anyhow::bail!("Switchboard API {status}: {code}: {message}");
        }
        Ok(value)
    }

    pub async fn issue_command(&self, command: IssueCommand) -> Result<()> {
        let (method, path, body) = issue_request(command);
        let result = self.request(method, &path, body).await?;
        println!("{}", serde_json::to_string_pretty(&result)?);
        Ok(())
    }
}

fn issue_request(command: IssueCommand) -> (Method, String, Option<Value>) {
    match command {
        IssueCommand::Create {
            title,
            description,
            kind,
            priority,
            backlog,
            demo,
        } => (
            Method::POST,
            "/api/v1/issues".into(),
            Some(json!({
                "title": title,
                "description": if demo { "demo".to_owned() } else { description },
                "kind": ledger::IssueKind::from(kind),
                "priority": priority,
                "backlog": backlog,
            })),
        ),
        IssueCommand::List { ready } => {
            (Method::GET, format!("/api/v1/issues?ready={ready}"), None)
        }
        IssueCommand::Show { id } => (Method::GET, format!("/api/v1/issues/{id}"), None),
        IssueCommand::Events { id } => (Method::GET, format!("/api/v1/issues/{id}/events"), None),
        IssueCommand::Attempts { id } => {
            (Method::GET, format!("/api/v1/issues/{id}/attempts"), None)
        }
        IssueCommand::Answer { id, answer } => (
            Method::POST,
            format!("/api/v1/issues/{id}/answer"),
            Some(json!({ "answer": answer })),
        ),
        IssueCommand::Complete { id } => (
            Method::POST,
            format!("/api/v1/issues/{id}/complete"),
            Some(json!({})),
        ),
        IssueCommand::Cancel { id, reason } => (
            Method::POST,
            format!("/api/v1/issues/{id}/cancel"),
            Some(json!({ "reason": reason })),
        ),
        IssueCommand::Ready { id } => (
            Method::POST,
            format!("/api/v1/issues/{id}/ready"),
            Some(json!({})),
        ),
        IssueCommand::Depend { id, dependency } => (
            Method::POST,
            format!("/api/v1/issues/{id}/dependencies"),
            Some(json!({ "dependency_id": dependency })),
        ),
    }
}
