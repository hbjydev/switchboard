//! Provider-independent execution of work claimed from the Ledger.

use std::future::Future;

use ledger::{Issue, IssueKind, Ledger, NewIssue, PeerId};

/// The durable agent identity is independent of its executor implementation.
#[derive(Clone, Debug)]
pub struct Agent {
    pub peer_id: PeerId,
}

/// Snapshot of durable context; executors do not maintain hidden task state.
pub struct ExecutionContext {
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

    /// Process one runnable issue, returning its persisted final state, or idle.
    /// Ledger errors propagate: no failed persistence is mistaken for successful work.
    pub async fn tick(&self) -> Result<Option<Issue>, ledger::Error> {
        let actor = self.agent.peer_id;
        let Some(issue) = self.ledger.claim_next_issue(actor).await? else {
            return Ok(None);
        };
        let issue = self.ledger.mark_running(issue.id, actor).await?;
        let context = ExecutionContext {
            children: self.ledger.children(issue.id).await?,
        };
        match self.executor.execute(&issue, context).await {
            ExecutionOutcome::Completed => {
                self.ledger.complete_issue(issue.id, actor).await?;
            }
            ExecutionOutcome::Failed { reason } => {
                self.ledger.fail_issue(issue.id, actor, &reason).await?;
            }
            ExecutionOutcome::CreatedChildWork {
                issue: child,
                blocking,
            } => {
                self.ledger
                    .create_child_issue(issue.id, actor, child, blocking)
                    .await?;
                if !blocking {
                    self.ledger.complete_issue(issue.id, actor).await?;
                }
            }
            ExecutionOutcome::NeedsHumanInput { title, description } => {
                self.ledger
                    .create_child_issue(
                        issue.id,
                        actor,
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
