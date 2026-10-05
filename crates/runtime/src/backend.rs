//! Autonomous agent execution, independent of model providers and persistence.
use std::{future::Future, path::PathBuf};

use ledger::{AttemptId, Issue, Peer};
pub use tokio_util::sync::CancellationToken;

/// A disposable invocation assembled from durable Ledger state by the Worker.
pub struct ExecutionRequest {
    pub attempt_id: AttemptId,
    pub peer: Peer,
    pub instructions: String,
    pub issue: Issue,
    pub parent: Option<Issue>,
    pub children: Vec<Issue>,
    pub dependencies: Vec<Issue>,
    pub workspace: Option<PathBuf>,
}

/// Only the Worker has authority to apply a result to the Ledger.
#[derive(Debug)]
pub enum ExecutionResult {
    Completed {
        summary: String,
    },
    Failed {
        reason: String,
    },
    Cancelled,
    /// Preserves the deterministic demo's existing handoff behavior.
    Demo(super::ExecutionOutcome),
}

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("backend I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("backend configuration: {0}")]
    Configuration(String),
    #[error("agent backend protocol: {0}")]
    Protocol(&'static str),
    #[error("agent backend process exited before execution completed")]
    UnexpectedExit,
    #[error("agent backend exceeded the execution deadline")]
    Timeout,
}

/// Run one autonomous attempt, not one LLM call. No database handles cross here.
pub trait AgentBackend: Send + Sync {
    fn execute(
        &self,
        request: ExecutionRequest,
        cancellation: CancellationToken,
    ) -> impl Future<Output = Result<ExecutionResult, BackendError>> + Send;
}

/// Compatibility for existing deterministic executors and controlled fixtures.
impl<T: super::Executor> AgentBackend for T {
    async fn execute(
        &self,
        request: ExecutionRequest,
        cancellation: CancellationToken,
    ) -> Result<ExecutionResult, BackendError> {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Ok(ExecutionResult::Cancelled),
            outcome = super::Executor::execute(self, &request.issue, super::ExecutionContext {
                attempt_id: request.attempt_id,
                children: request.children,
            }) => Ok(ExecutionResult::Demo(outcome)),
        }
    }
}

/// Cancel on future drop too, including a Worker tick abandoned by its caller.
pub struct CancelOnDrop(pub CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
