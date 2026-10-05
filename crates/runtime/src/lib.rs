//! Provider-independent execution of work claimed from the Ledger.

use std::future::Future;

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
pub enum ExecutionOutcome {
    Completed,
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
}

impl<E: Executor> Worker<E> {
    #[must_use]
    pub const fn new(ledger: Ledger, agent: Agent, executor: E) -> Self {
        Self {
            ledger,
            agent,
            executor,
        }
    }

    /// Recover expired work and process one issue, returning its persisted state, or idle.
    /// Ledger errors propagate: no failed persistence is mistaken for successful work.
    pub async fn tick(&self) -> Result<Option<Issue>, ledger::Error> {
        self.ledger.recover_expired_attempts().await?;
        let Some(claim) = self.ledger.claim_next_issue(self.agent.peer_id).await? else {
            return Ok(None);
        };
        let attempt_id = claim.attempt.id;
        let issue = self.ledger.mark_running(attempt_id).await?;
        let execution = async {
            let context = ExecutionContext {
                attempt_id,
                children: self.ledger.children(issue.id).await?,
            };
            Ok::<_, ledger::Error>(self.executor.execute(&issue, context).await)
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
                _ = heartbeats.tick() => {
                    self.ledger.heartbeat(attempt_id).await?;
                }
                result = &mut execution => break result?,
            }
        };

        match outcome {
            ExecutionOutcome::Completed => {
                return self.ledger.complete_attempt(attempt_id).await.map(Some);
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
