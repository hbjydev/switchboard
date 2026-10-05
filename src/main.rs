use std::{path::PathBuf, time::Duration};

use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};
use ledger::{IssueId, IssueKind, Ledger, NewIssue, PeerKind};
use runtime::environment::LocalProcessEnvironment;
use runtime::{
    Agent, AgentBackend, BackendError, CancellationToken, ExecutionRequest, ExecutionResult,
    FakeExecutor, PiBackend, PiConfig, Worker,
};

#[derive(Parser)]
#[command(about = "A Ledger-driven autonomous worker runtime", version)]
struct Cli {
    #[arg(long, env = "DATABASE_URL")]
    database_url: String,
    #[arg(long, default_value = "operator")]
    human: String,
    /// Execution lease length; workers renew active attempts automatically.
    #[arg(
        long,
        global = true,
        env = "SWITCHBOARD_LEASE_SECONDS",
        default_value_t = 30,
        value_parser = clap::value_parser!(u64).range(1..=86_400)
    )]
    lease_seconds: u64,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Apply the new Ledger database migrations.
    Migrate,
    Issue {
        #[command(subcommand)]
        command: IssueCommand,
    },
    Worker {
        #[command(subcommand)]
        command: WorkerCommand,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum Kind {
    Task,
    Question,
    Approval,
}

impl From<Kind> for IssueKind {
    fn from(value: Kind) -> Self {
        match value {
            Kind::Task => Self::Task,
            Kind::Question => Self::Question,
            Kind::Approval => Self::Approval,
        }
    }
}

#[derive(Subcommand)]
enum IssueCommand {
    Create {
        #[arg(long)]
        title: String,
        #[arg(long, default_value = "")]
        description: String,
        #[arg(long, value_enum, default_value = "task")]
        kind: Kind,
        #[arg(long, default_value_t = 0)]
        priority: i32,
        #[arg(long)]
        backlog: bool,
        /// Deterministic child-work and human-question lifecycle.
        #[arg(long, conflicts_with = "description")]
        demo: bool,
    },
    List {
        #[arg(long)]
        ready: bool,
    },
    Show {
        id: IssueId,
    },
    Events {
        id: IssueId,
    },
    Attempts {
        id: IssueId,
    },
    Answer {
        id: IssueId,
        #[arg(long)]
        answer: String,
    },
    Complete {
        id: IssueId,
    },
    Cancel {
        id: IssueId,
        #[arg(long)]
        reason: String,
    },
    Ready {
        id: IssueId,
    },
    Depend {
        id: IssueId,
        dependency: IssueId,
    },
}

#[derive(Subcommand)]
enum WorkerCommand {
    /// Recover expired execution attempts and reconsider their issues.
    Recover,
    Run(WorkerOptions),
}

#[derive(Clone, Copy, ValueEnum)]
enum BackendKind {
    Fake,
    Pi,
}

#[derive(Args)]
struct WorkerOptions {
    #[arg(long, default_value = "worker")]
    agent: String,
    /// Stop once no issue is runnable (human questions remain pending).
    #[arg(long)]
    until_idle: bool,
    #[arg(long, value_enum, default_value = "fake")]
    backend: BackendKind,
    /// Project directory for Pi resource discovery and coding tools.
    #[arg(long, env = "SWITCHBOARD_WORKSPACE", required_if_eq("backend", "pi"))]
    workspace: Option<PathBuf>,
    #[arg(long, env = "SWITCHBOARD_PI_BINARY", default_value = "pi")]
    pi_binary: String,
    #[arg(long, env = "SWITCHBOARD_PI_PROVIDER")]
    pi_provider: Option<String>,
    #[arg(long, env = "SWITCHBOARD_PI_MODEL")]
    pi_model: Option<String>,
    #[arg(
        long,
        env = "SWITCHBOARD_AGENT_INSTRUCTIONS",
        default_value = "Complete the assigned task and report its result."
    )]
    instructions: String,
    #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(u64).range(1..))]
    execution_timeout_seconds: u64,
}

enum SelectedBackend {
    Fake(FakeExecutor),
    Pi(PiBackend),
}
impl AgentBackend for SelectedBackend {
    async fn execute(
        &self,
        request: ExecutionRequest,
        cancellation: CancellationToken,
    ) -> Result<ExecutionResult, BackendError> {
        match self {
            Self::Fake(backend) => AgentBackend::execute(backend, request, cancellation).await,
            Self::Pi(backend) => backend.execute(request, cancellation).await,
        }
    }
}

fn selected_backend(options: &WorkerOptions) -> Result<SelectedBackend> {
    match options.backend {
        BackendKind::Fake => Ok(SelectedBackend::Fake(FakeExecutor)),
        BackendKind::Pi => {
            let workspace = options
                .workspace
                .clone()
                .ok_or_else(|| anyhow::anyhow!("--backend pi requires --workspace"))?;
            anyhow::ensure!(
                workspace.is_dir(),
                "Pi workspace must be an existing directory"
            );
            let mut config = PiConfig::local(workspace);
            config.binary = (&options.pi_binary).into();
            config.provider.clone_from(&options.pi_provider);
            config.model.clone_from(&options.pi_model);
            config.execution_timeout = Duration::from_secs(options.execution_timeout_seconds);
            Ok(SelectedBackend::Pi(PiBackend::new(
                config,
                LocalProcessEnvironment,
            )?))
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "runtime=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    // Validate infrastructure configuration before opening the database.
    let backend = if let Command::Worker {
        command: WorkerCommand::Run(options),
    } = &cli.command
    {
        Some(selected_backend(options)?)
    } else {
        None
    };
    let ledger = Ledger::connect(&cli.database_url)
        .await?
        .with_lease_duration(Duration::from_secs(cli.lease_seconds))?;
    match cli.command {
        Command::Migrate => {
            ledger.migrate().await?;
            println!("Ledger migrations applied");
        }
        Command::Issue { command } => issue_command(&ledger, &cli.human, command).await?,
        Command::Worker {
            command: WorkerCommand::Recover,
        } => {
            for id in ledger.recover_expired_attempts().await? {
                println!("Recovered {id}");
            }
        }
        Command::Worker {
            command: WorkerCommand::Run(options),
        } => {
            let backend =
                backend.ok_or_else(|| anyhow::anyhow!("worker backend was not configured"))?;
            run_worker(ledger, options, backend).await?;
        }
    }
    Ok(())
}

async fn run_worker(
    ledger: Ledger,
    options: WorkerOptions,
    backend: SelectedBackend,
) -> Result<()> {
    let peer = ledger.ensure_peer(&options.agent, PeerKind::Agent).await?;
    let worker = Worker::new(ledger, Agent { peer_id: peer.id }, backend)
        .with_execution_settings(options.instructions, options.workspace);
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    loop {
        let cancellation = CancellationToken::new();
        let tick = worker.tick_with_cancellation(cancellation.clone());
        tokio::pin!(tick);
        let result = tokio::select! {
            biased;
            signal = &mut shutdown => {
                signal?;
                cancellation.cancel();
                let _ = tick.await; // Wait for abort/cleanup; do not apply cancelled output.
                break;
            }
            result = &mut tick => result?,
        };
        match result {
            Some(issue) => println!("{} {:?}: {}", issue.id, issue.status, issue.title),
            None if options.until_idle => break,
            None => tokio::select! {
                signal = &mut shutdown => { signal?; break; }
                () = tokio::time::sleep(Duration::from_millis(500)) => {}
            },
        }
    }
    Ok(())
}

async fn issue_command(ledger: &Ledger, human_name: &str, command: IssueCommand) -> Result<()> {
    let human = ledger.ensure_peer(human_name, PeerKind::Human).await?;
    match command {
        IssueCommand::Create {
            title,
            description,
            kind,
            priority,
            backlog,
            demo,
        } => {
            let issue = ledger
                .create_issue(
                    human.id,
                    NewIssue {
                        title,
                        description: if demo { "demo".to_owned() } else { description },
                        kind: kind.into(),
                        priority,
                        backlog,
                    },
                )
                .await?;
            println!("Created {}", issue.id);
        }
        IssueCommand::List { ready } => {
            let issues = if ready {
                ledger.list_ready_issues().await?
            } else {
                ledger.list_issues().await?
            };
            for issue in issues {
                println!(
                    "{} {:?} {:?}: {}",
                    issue.id, issue.kind, issue.status, issue.title
                );
            }
        }
        IssueCommand::Show { id } => println!(
            "{}",
            serde_json::to_string_pretty(&ledger.get_issue(id).await?)?
        ),
        IssueCommand::Events { id } => println!(
            "{}",
            serde_json::to_string_pretty(&ledger.events(id).await?)?
        ),
        IssueCommand::Attempts { id } => print_attempts(ledger, id).await?,
        IssueCommand::Answer { id, answer } => {
            ledger.resolve_human_issue(id, human.id, &answer).await?;
            println!("Answered {id}");
        }
        IssueCommand::Complete { id } => {
            ledger.complete_issue(id, human.id).await?;
            println!("Completed {id}");
        }
        IssueCommand::Cancel { id, reason } => {
            ledger.cancel_issue(id, human.id, &reason).await?;
            println!("Cancelled {id}");
        }
        IssueCommand::Ready { id } => {
            let issue = ledger.make_ready(id, human.id).await?;
            println!("{} {:?}", issue.id, issue.status);
        }
        IssueCommand::Depend { id, dependency } => {
            ledger.add_dependency(id, dependency, human.id).await?;
            println!("{id} depends on {dependency}");
        }
    }
    Ok(())
}

async fn print_attempts(ledger: &Ledger, id: IssueId) -> Result<()> {
    let issue = ledger.get_issue(id).await?;
    let mut records = Vec::new();
    for attempt in ledger.attempts(id).await? {
        let peer = ledger.peer(attempt.peer_id).await?;
        records.push(serde_json::json!({
            "current": issue.current_attempt_id == Some(attempt.id),
            "peer": peer,
            "attempt": attempt,
        }));
    }
    println!("{}", serde_json::to_string_pretty(&records)?);
    Ok(())
}
