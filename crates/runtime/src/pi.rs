//! Pi RPC adapter. Protocol reference: Pi's docs/rpc.md, rpc-commands.md, json.md.
use std::{ffi::OsString, fmt::Write as _, path::PathBuf, time::Duration};

use serde_json::{Value, json};

use crate::environment::{ExecutionEnvironment, LocalProcessEnvironment, ProcessSpec, RpcProcess};
use crate::{AgentBackend, BackendError, CancellationToken, ExecutionRequest, ExecutionResult};

pub const MAX_SUMMARY_BYTES: usize = 8192;
pub const MAX_PROMPT_BYTES: usize = 32768;

pub struct PiConfig {
    pub binary: OsString,
    pub workspace: PathBuf,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub execution_timeout: Duration,
    pub shutdown_grace: Duration,
}

impl PiConfig {
    #[must_use]
    pub fn local(workspace: PathBuf) -> Self {
        Self {
            binary: "pi".into(),
            workspace,
            provider: None,
            model: None,
            execution_timeout: Duration::from_secs(3600),
            shutdown_grace: Duration::from_secs(2),
        }
    }
}

pub struct PiBackend<E = LocalProcessEnvironment> {
    config: PiConfig,
    environment: E,
}

impl<E: ExecutionEnvironment> PiBackend<E> {
    pub fn new(config: PiConfig, environment: E) -> Result<Self, BackendError> {
        if config.binary.is_empty()
            || config.execution_timeout.is_zero()
            || config.shutdown_grace.is_zero()
        {
            return Err(BackendError::Configuration(
                "Pi binary and positive execution/shutdown deadlines are required".into(),
            ));
        }
        Ok(Self {
            config,
            environment,
        })
    }

    fn process_spec(&self, request: &ExecutionRequest) -> ProcessSpec {
        let mut arguments: Vec<OsString> =
            ["--mode", "rpc", "--no-session"].map(OsString::from).into();
        for (flag, value) in [
            ("--provider", &self.config.provider),
            ("--model", &self.config.model),
        ] {
            if let Some(value) = value {
                arguments.extend([flag.into(), value.into()]);
            }
        }
        ProcessSpec {
            attempt_id: request.attempt_id,
            binary: self.config.binary.clone(),
            arguments,
            workspace: request
                .workspace
                .clone()
                .unwrap_or_else(|| self.config.workspace.clone()),
            shutdown_grace: self.config.shutdown_grace,
            abort_record: Some(json!({"id":"abort", "type":"abort"}).to_string()),
        }
    }
}

impl<E: ExecutionEnvironment> AgentBackend for PiBackend<E> {
    async fn execute(
        &self,
        request: ExecutionRequest,
        cancellation: CancellationToken,
    ) -> Result<ExecutionResult, BackendError> {
        if cancellation.is_cancelled() {
            return Ok(ExecutionResult::Cancelled);
        }
        let deadline = tokio::time::Instant::now() + self.config.execution_timeout;
        let mut process = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Ok(ExecutionResult::Cancelled),
            result = tokio::time::timeout_at(deadline, self.environment.spawn(self.process_spec(&request))) => {
                result.unwrap_or(Err(BackendError::Timeout)).inspect_err(|_error| {
                    tracing::warn!(attempt_id = %request.attempt_id, "Pi startup failed");
                })?
            }
        };
        let invocation = run_rpc(&mut process, &request);
        let result = tokio::select! {
            biased;
            () = cancellation.cancelled() => Ok(ExecutionResult::Cancelled),
            result = tokio::time::timeout_at(deadline, invocation) => result.unwrap_or(Err(BackendError::Timeout)),
        };
        let abort = !matches!(
            result,
            Ok(ExecutionResult::Completed { .. } | ExecutionResult::Failed { .. })
        );
        let cleanup = process.finish(abort).await;
        if result.is_err() {
            tracing::warn!(attempt_id = %request.attempt_id, "Pi backend failure");
        }
        // Never return a completion while its process is still alive.
        if !matches!(result, Ok(ExecutionResult::Cancelled)) {
            cleanup?;
        }
        result
    }
}

async fn run_rpc<P: RpcProcess>(
    process: &mut P,
    request: &ExecutionRequest,
) -> Result<ExecutionResult, BackendError> {
    let id = request.attempt_id.to_string();
    process
        .send(json!({"id":id,"type":"prompt","message":build_prompt(request)}).to_string())
        .await?;
    let mut state = RpcState::default();
    loop {
        let line = process
            .next_record()
            .await?
            .ok_or(BackendError::UnexpectedExit)?;
        let record: Value = serde_json::from_str(&line)
            .map_err(|_error| BackendError::Protocol("malformed JSON"))?;
        state.observe(&record, &id)?;
        if state.accepted && state.settled {
            tracing::info!(attempt_id = %request.attempt_id, "Pi agent settled");
            return state.result();
        }
    }
}

#[derive(Default)]
struct RpcState {
    accepted: bool,
    settled: bool,
    last_message: Option<(String, String)>,
    retry_failed: bool,
}

impl RpcState {
    fn observe(&mut self, record: &Value, id: &str) -> Result<(), BackendError> {
        match record
            .get("type")
            .and_then(Value::as_str)
            .ok_or(BackendError::Protocol("record missing type"))?
        {
            "response" => self.response(record, id)?,
            "message_end" => self.message(record)?,
            "agent_settled" => self.settled = true,
            "auto_retry_end" => {
                self.retry_failed = record.get("success").and_then(Value::as_bool) == Some(false);
            }
            "extension_ui_request" => {
                return Err(BackendError::Protocol(
                    "interactive Pi extensions are unsupported",
                ));
            }
            _ => {} // Consume tool/compaction/streaming events without recording conversations.
        }
        Ok(())
    }

    fn response(&mut self, record: &Value, id: &str) -> Result<(), BackendError> {
        let success = record
            .get("success")
            .and_then(Value::as_bool)
            .ok_or(BackendError::Protocol("response missing success"))?;
        if !success {
            return Err(BackendError::Protocol("Pi rejected an RPC command"));
        }
        if record.get("id").and_then(Value::as_str) == Some(id) {
            if record.get("command").and_then(Value::as_str) != Some("prompt") {
                return Err(BackendError::Protocol("prompt response command mismatch"));
            }
            if record.pointer("/data/disposition").and_then(Value::as_str) == Some("handled") {
                return Err(BackendError::Protocol(
                    "prompt was handled without starting execution",
                ));
            }
            self.accepted = true;
            tracing::info!(attempt_id = id, "Pi prompt accepted");
        }
        Ok(())
    }

    fn message(&mut self, record: &Value) -> Result<(), BackendError> {
        let message = record
            .get("message")
            .ok_or(BackendError::Protocol("message_end missing message"))?;
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            return Ok(());
        }
        let reason = message
            .get("stopReason")
            .and_then(Value::as_str)
            .ok_or(BackendError::Protocol("assistant missing stopReason"))?;
        let blocks = message
            .get("content")
            .and_then(Value::as_array)
            .ok_or(BackendError::Protocol("assistant missing content"))?;
        let mut text = String::new();
        for block in blocks {
            if block.get("type").and_then(Value::as_str) == Some("text") {
                let value = block
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or(BackendError::Protocol("text block missing text"))?;
                append_bounded(&mut text, value, MAX_SUMMARY_BYTES);
            }
        }
        self.last_message = Some((reason.to_owned(), text));
        Ok(())
    }

    fn result(self) -> Result<ExecutionResult, BackendError> {
        if self.retry_failed {
            return Ok(ExecutionResult::Failed {
                reason: "Pi exhausted its execution retries".into(),
            });
        }
        let (reason, text) = self.last_message.ok_or(BackendError::Protocol(
            "settled without an assistant result",
        ))?;
        match reason.as_str() {
            "error" => Ok(ExecutionResult::Failed {
                reason: "Pi agent execution failed".into(),
            }),
            "aborted" => Ok(ExecutionResult::Cancelled),
            "length" => Ok(ExecutionResult::Failed {
                reason: "Pi final response exceeded its model output limit".into(),
            }),
            "stop" if !text.trim().is_empty() => {
                let failure = text
                    .trim()
                    .strip_prefix("SWITCHBOARD_FAILED:")
                    .map(|reason| {
                        if reason.trim().is_empty() {
                            "Pi could not complete the task".into()
                        } else {
                            reason.trim().to_owned()
                        }
                    });
                Ok(
                    failure.map_or(ExecutionResult::Completed { summary: text }, |reason| {
                        ExecutionResult::Failed { reason }
                    }),
                )
            }
            _ => Err(BackendError::Protocol(
                "settled without a final completion summary",
            )),
        }
    }
}

pub(crate) fn append_bounded(output: &mut String, value: &str, maximum: usize) {
    let mut end = value.len().min(maximum.saturating_sub(output.len()));
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    if let Some(prefix) = value.get(..end) {
        output.push_str(prefix);
    }
}

/// Bound the snapshot and keep coordination instructions ahead of task data.
#[must_use]
pub fn build_prompt(request: &ExecutionRequest) -> String {
    let mut prompt = format!(
        "You are agent {} (Peer {}). Issue {}. Disposable execution attempt {}.\n\
         Switchboard owns durable coordination; agent backends own ephemeral agent execution.\n\
         Work on the requested task using your normal coding tools. Do not invent a durable task queue,\n\
         mutate the Switchboard Ledger, delegate Issues, or create human Questions.\n\
         Session files are not durable work state. Return a concise final completion summary.\n\
         If you cannot complete the task, begin your final summary with SWITCHBOARD_FAILED:.\n\
         Context fields may be truncated; related lists include at most 8 Issues.\nInstructions:\n",
        bounded(&request.peer.name, 256),
        request.peer.id,
        request.issue.id,
        request.attempt_id,
    );
    prompt.push_str(&bounded(&request.instructions, 4096));
    prompt.push_str("\nTask:\n");
    prompt.push_str(&issue_context(&request.issue, 12288));
    if let Some(parent) = &request.parent {
        prompt.push_str("\nParent:\n");
        prompt.push_str(&issue_context(parent, 1024));
    }
    for (label, issues) in [
        ("Children", &request.children),
        ("Dependencies", &request.dependencies),
    ] {
        let _ = write!(prompt, "\n{label}:\n");
        for issue in issues.iter().take(8) {
            append_bounded(&mut prompt, &issue_context(issue, 512), MAX_PROMPT_BYTES);
        }
    }
    bounded(&prompt, MAX_PROMPT_BYTES)
}

fn bounded(value: &str, maximum: usize) -> String {
    let mut result = String::new();
    append_bounded(&mut result, value, maximum);
    result
}
fn issue_context(issue: &ledger::Issue, description_limit: usize) -> String {
    format!(
        "{} {:?} {:?}: {}\n{}\n",
        issue.id,
        issue.kind,
        issue.status,
        bounded(&issue.title, 512),
        bounded(&issue.description, description_limit)
    )
}
