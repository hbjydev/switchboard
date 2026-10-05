use std::{path::PathBuf, time::Duration};

mod auth;
mod client;
mod server;

use anyhow::Result;
use clap::{Args, Parser, Subcommand, ValueEnum};
use control_plane::Application;
use ledger::{IssueId, IssueKind, Ledger, NewIssue, PeerKind};
use runtime::environment::LocalProcessEnvironment;
use runtime::{
    Agent, AgentBackend, BackendError, CancellationToken, ExecutionRequest, ExecutionResult,
    FakeExecutor, PiBackend, PiConfig, Worker,
};

#[derive(Parser)]
#[command(
    about = "A persistent Ledger control plane and autonomous worker runtime",
    version
)]
struct Cli {
    #[arg(long, env = "DATABASE_URL")]
    database_url: Option<String>,
    /// Use the persistent control plane; issue commands then need no database access.
    #[arg(long, env = "SWITCHBOARD_URL")]
    url: Option<String>,
    #[command(flatten)]
    auth: auth::ClientAuthOptions,
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
    Auth {
        #[command(subcommand)]
        command: auth::AuthCommand,
    },
    /// Run the HTTP control plane and optional autonomous scheduler.
    Serve(server::ServeOptions),
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
    #[value(name = "pi-local", alias = "pi")]
    Pi,
}

#[derive(Args)]
struct WorkerOptions {
    /// Stop once no issue is runnable (human questions remain pending).
    #[arg(long)]
    until_idle: bool,
    #[command(flatten)]
    execution: ExecutionOptions,
}

#[derive(Args)]
struct ExecutionOptions {
    #[arg(long, env = "SWITCHBOARD_AGENT_NAME", default_value = "worker")]
    agent: String,
    #[arg(long, env = "SWITCHBOARD_BACKEND", value_enum, default_value = "fake")]
    backend: BackendKind,
    /// Project directory for Pi resource discovery and coding tools.
    #[arg(long, env = "SWITCHBOARD_WORKSPACE")]
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

fn selected_backend(options: &ExecutionOptions) -> Result<SelectedBackend> {
    match options.backend {
        BackendKind::Fake => Ok(SelectedBackend::Fake(FakeExecutor)),
        BackendKind::Pi => {
            let workspace = options
                .workspace
                .clone()
                .ok_or_else(|| anyhow::anyhow!("--backend pi-local requires --workspace"))?;
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
                .unwrap_or_else(|_| "switchboard=info,control_plane=info,runtime=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    if let Command::Auth { command } = cli.command {
        return cli.auth.execute(command).await;
    }
    if let Command::Issue { command } = cli.command {
        if let Some(url) = cli.url {
            let token = cli.auth.access_token().await?;
            let client = client::ApiClient::new(&url, token)?;
            return client.issue_command(command).await;
        }
        let ledger = configured_ledger(cli.database_url.as_deref(), cli.lease_seconds).await?;
        return issue_command(&Application::new(ledger), &cli.human, command).await;
    }
    execute_infrastructure(cli).await
}

async fn configured_ledger(database_url: Option<&str>, lease_seconds: u64) -> Result<Ledger> {
    let url = database_url.ok_or_else(|| {
        anyhow::anyhow!("DATABASE_URL or --database-url is required for this command; use SWITCHBOARD_URL for remote issue commands")
    })?;
    Ok(Ledger::connect(url)
        .await?
        .with_lease_duration(Duration::from_secs(lease_seconds))?)
}

async fn execute_infrastructure(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Serve(options) => {
            // API-only mode deliberately needs no Pi configuration.
            let backend = options.selected_backend()?;
            let ledger = configured_ledger(cli.database_url.as_deref(), cli.lease_seconds).await?;
            server::serve(ledger, options, backend, cli.auth.oidc_allow_loopback_http).await?;
        }
        Command::Migrate => {
            let ledger = configured_ledger(cli.database_url.as_deref(), cli.lease_seconds).await?;
            ledger.migrate().await?;
            println!("Ledger migrations applied");
        }
        Command::Worker {
            command: WorkerCommand::Recover,
        } => {
            let ledger = configured_ledger(cli.database_url.as_deref(), cli.lease_seconds).await?;
            for id in ledger.recover_expired_attempts().await? {
                println!("Recovered {id}");
            }
        }
        Command::Worker {
            command: WorkerCommand::Run(options),
        } => {
            let backend = selected_backend(&options.execution)?;
            let ledger = configured_ledger(cli.database_url.as_deref(), cli.lease_seconds).await?;
            run_worker(ledger, options, backend).await?;
        }
        Command::Issue { .. } | Command::Auth { .. } => {}
    }
    Ok(())
}

async fn run_worker(
    ledger: Ledger,
    options: WorkerOptions,
    backend: SelectedBackend,
) -> Result<()> {
    let peer = ledger
        .ensure_peer(&options.execution.agent, PeerKind::Agent)
        .await?;
    let worker = Worker::new(ledger, Agent { peer_id: peer.id }, backend)
        .with_execution_settings(options.execution.instructions, options.execution.workspace);
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

async fn issue_command(app: &Application, human_name: &str, command: IssueCommand) -> Result<()> {
    match command {
        IssueCommand::Create {
            title,
            description,
            kind,
            priority,
            backlog,
            demo,
        } => {
            let issue = app
                .create(
                    human_name,
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
            let issues = app.list(ready).await?;
            for issue in issues {
                println!(
                    "{} {:?} {:?}: {}",
                    issue.id, issue.kind, issue.status, issue.title
                );
            }
        }
        IssueCommand::Show { id } => {
            println!("{}", serde_json::to_string_pretty(&app.get(id).await?)?);
        }
        IssueCommand::Events { id } => {
            println!("{}", serde_json::to_string_pretty(&app.events(id).await?)?);
        }
        IssueCommand::Attempts { id } => print_attempts(app, id).await?,
        IssueCommand::Answer { id, answer } => {
            app.answer(human_name, id, &answer).await?;
            println!("Answered {id}");
        }
        IssueCommand::Complete { id } => {
            app.complete(human_name, id).await?;
            println!("Completed {id}");
        }
        IssueCommand::Cancel { id, reason } => {
            app.cancel(human_name, id, &reason).await?;
            println!("Cancelled {id}");
        }
        IssueCommand::Ready { id } => {
            let issue = app.ready(human_name, id).await?;
            println!("{} {:?}", issue.id, issue.status);
        }
        IssueCommand::Depend { id, dependency } => {
            app.depend(human_name, id, dependency).await?;
            println!("{id} depends on {dependency}");
        }
    }
    Ok(())
}

async fn print_attempts(app: &Application, id: IssueId) -> Result<()> {
    let records: Vec<_> = app
        .attempts(id)
        .await?
        .into_iter()
        .map(control_plane::http::dto::AttemptRecordResponse::from)
        .collect();
    println!("{}", serde_json::to_string_pretty(&records)?);
    Ok(())
}
