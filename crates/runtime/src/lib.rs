//! Provider-independent execution of work claimed from the Ledger.

mod backend;
pub mod environment;
pub mod pi;
pub use backend::{
    AgentBackend, BackendError, CancellationToken, ExecutionRequest, ExecutionResult,
};
pub use pi::{PiBackend, PiConfig};

use std::{future::Future, path::PathBuf, time::Duration};

use ledger::{AttemptId, Issue, IssueKind, Ledger, NewIssue, PeerId};

/// The durable agent identity is independent of its executor implementation.
#[derive(Clone, Debug)]
pub struct Agent {
    pub peer_id: PeerId,
}

/// Snapshot of durable context; executors do not maintain hidden task state.
pub struct ExecutionContext {
    /// Durable scope for future side effects: `(attempt_id, operation identity)`.
    pub attempt_id: AttemptId,
    pub children: Vec<Issue>,
}

/// An executor describes its outcome; the worker applies it through Ledger rules.
#[derive(Debug)]
pub enum ExecutionOutcome {
    Completed,
    CompletedWithSummary { summary: String },
    Failed { reason: String },
    CreatedChildWork { issue: NewIssue, blocking: bool },
    NeedsHumanInput { title: String, description: String },
}

/// No provider or model payloads are part of this boundary.
pub trait Executor: Send + Sync {
    fn execute(
        &self,
        issue: &Issue,
        context: ExecutionContext,
    ) -> impl Future<Output = ExecutionOutcome> + Send;
}

/// Deterministic fixtures: description `demo` runs the child/question lifecycle;
/// `fail: reason` produces an explicit failure. Other tasks complete immediately.
pub struct FakeExecutor;

impl Executor for FakeExecutor {
    async fn execute(&self, issue: &Issue, context: ExecutionContext) -> ExecutionOutcome {
        if let Some(reason) = issue.description.strip_prefix("fail:") {
            return ExecutionOutcome::Failed {
                reason: if reason.trim().is_empty() {
                    "Fake executor requested failure".to_owned()
                } else {
                    reason.trim().to_owned()
                },
            };
        }
        if issue.description == "demo" {
            if !context
                .children
                .iter()
                .any(|child| child.kind == IssueKind::Task)
            {
                return ExecutionOutcome::CreatedChildWork {
                    issue: NewIssue::task("Prepare demo implementation"),
                    blocking: true,
                };
            }
            if !context
                .children
                .iter()
                .any(|child| child.kind == IssueKind::Question)
            {
                return ExecutionOutcome::NeedsHumanInput {
                    title: "Which option should the demo use?".to_owned(),
                    description: "Answer with a choice, for example: Use option A".to_owned(),
                };
            }
        }
        ExecutionOutcome::Completed
    }
}

pub struct Worker<E> {
    ledger: Ledger,
    agent: Agent,
    executor: E,
    instructions: String,
    workspace: Option<PathBuf>,
}

impl<E: AgentBackend> Worker<E> {
    #[must_use]
    pub fn new(ledger: Ledger, agent: Agent, executor: E) -> Self {
        Self {
            ledger,
            agent,
            executor,
            instructions: "Complete the assigned task and report its result.".into(),
            workspace: None,
        }
    }

    #[must_use]
    pub fn with_execution_settings(
        mut self,
        instructions: String,
        workspace: Option<PathBuf>,
    ) -> Self {
        self.instructions = instructions;
        self.workspace = workspace;
        self
    }

    /// Recover expired work and process one issue, returning its persisted state, or idle.
    /// Ledger errors propagate: no failed persistence is mistaken for successful work.
    pub async fn tick(&self) -> Result<Option<Issue>, ledger::Error> {
        self.tick_with_cancellation(CancellationToken::new()).await
    }

    pub async fn tick_with_cancellation(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Option<Issue>, ledger::Error> {
        let cancellation = cancellation.child_token();
        if cancellation.is_cancelled() {
            return Ok(None);
        }
        let _cancel_on_drop = backend::CancelOnDrop(cancellation.clone());
        let claim = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Ok(None),
            claim = self.recover_and_claim(&cancellation) => claim?,
        };
        let Some(claim) = claim else {
            return Ok(None);
        };
        let attempt_id = claim.attempt.id;
        let issue = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(ledger::Error::ExecutionLost(attempt_id)),
            issue = self.ledger.mark_running(attempt_id) => issue?,
        };
        tracing::info!(%attempt_id, issue_id = %issue.id, "execution started");
        let execution = async {
            let request = self.load_request(&issue, attempt_id).await?;
            Ok::<_, ledger::Error>(self.executor.execute(request, cancellation.clone()).await)
        };

        // The timer lives in this tick: canceling the execution also stops renewal.
        // Delay the first heartbeat, since claiming already established a lease.
        let period = self.ledger.lease_duration() / 3;
        let mut heartbeats = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        heartbeats.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tokio::pin!(execution);
        let outcome = loop {
            tokio::select! {
                biased;
                () = cancellation.cancelled() => {
                    let _ = tokio::time::timeout(Duration::from_secs(5), &mut execution).await;
                    return Err(ledger::Error::ExecutionLost(attempt_id));
                }
                _ = heartbeats.tick() => {
                    let renewal = tokio::select! {
                        biased;
                        () = cancellation.cancelled() => Err(ledger::Error::ExecutionLost(attempt_id)),
                        renewal = self.ledger.heartbeat(attempt_id) => renewal,
                    };
                    if let Err(error) = renewal {
                        cancellation.cancel();
                        // Give a cooperative backend time to abort and reap. On timeout,
                        // dropping its future still triggers the process supervisor.
                        let _ = tokio::time::timeout(Duration::from_secs(5), &mut execution).await;
                        return Err(error);
                    }
                }
                result = &mut execution => break result?,
            }
        };

        if cancellation.is_cancelled() {
            return Err(ledger::Error::ExecutionLost(attempt_id));
        }
        self.apply_result(&issue, attempt_id, outcome).await
    }

    async fn recover_and_claim(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<Option<ledger::Claim>, ledger::Error> {
        self.ledger.recover_expired_attempts().await?;
        if cancellation.is_cancelled() {
            return Ok(None);
        }
        self.ledger.claim_next_issue(self.agent.peer_id).await
    }

    async fn load_request(
        &self,
        issue: &Issue,
        attempt_id: AttemptId,
    ) -> Result<ExecutionRequest, ledger::Error> {
        Ok(ExecutionRequest {
            attempt_id,
            peer: self
                .ledger
                .peer(self.agent.peer_id)
                .await?
                .ok_or(ledger::Error::IneligibleActor)?,
            instructions: self.instructions.clone(),
            issue: issue.clone(),
            parent: match issue.parent_id {
                Some(id) => Some(self.ledger.get_issue(id).await?),
                None => None,
            },
            children: self.ledger.children(issue.id).await?,
            dependencies: self.ledger.dependencies(issue.id).await?,
            workspace: self.workspace.clone(),
        })
    }

    async fn apply_result(
        &self,
        issue: &Issue,
        attempt_id: AttemptId,
        outcome: Result<ExecutionResult, BackendError>,
    ) -> Result<Option<Issue>, ledger::Error> {
        let outcome = match outcome {
            Ok(ExecutionResult::Completed { summary }) => {
                ExecutionOutcome::CompletedWithSummary { summary }
            }
            Ok(ExecutionResult::Failed { reason }) => ExecutionOutcome::Failed { reason },
            Ok(ExecutionResult::Demo(outcome)) => outcome,
            Ok(ExecutionResult::Cancelled) => return Err(ledger::Error::ExecutionLost(attempt_id)),
            Err(error) => ExecutionOutcome::Failed {
                reason: error.to_string(),
            },
        };
        match outcome {
            ExecutionOutcome::Completed => {
                return self.ledger.complete_attempt(attempt_id).await.map(Some);
            }
            ExecutionOutcome::CompletedWithSummary { summary } => {
                return self
                    .ledger
                    .complete_attempt_with_summary(attempt_id, &summary)
                    .await
                    .map(Some);
            }
            ExecutionOutcome::Failed { reason } => {
                return self
                    .ledger
                    .fail_attempt(attempt_id, &reason)
                    .await
                    .map(Some);
            }
            ExecutionOutcome::CreatedChildWork {
                issue: child,
                blocking,
            } => {
                self.ledger
                    .create_child_for_attempt(attempt_id, child, blocking)
                    .await?;
            }
            ExecutionOutcome::NeedsHumanInput { title, description } => {
                self.ledger
                    .create_child_for_attempt(
                        attempt_id,
                        NewIssue {
                            title,
                            description,
                            kind: IssueKind::Question,
                            priority: issue.priority,
                            backlog: false,
                        },
                        true,
                    )
                    .await?;
            }
        }
        self.ledger.get_issue(issue.id).await.map(Some)
    }
}
