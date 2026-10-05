use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};
use ledger::{IssueId, IssueKind, Ledger, NewIssue, PeerKind};
use runtime::{Agent, FakeExecutor, Worker};

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
    Run {
        #[arg(long, default_value = "worker")]
        agent: String,
        /// Stop once no issue is runnable (human questions remain pending).
        #[arg(long)]
        until_idle: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
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
            command: WorkerCommand::Run { agent, until_idle },
        } => {
            let peer = ledger.ensure_peer(&agent, PeerKind::Agent).await?;
            let worker = Worker::new(ledger, Agent { peer_id: peer.id }, FakeExecutor);
            let shutdown = tokio::signal::ctrl_c();
            tokio::pin!(shutdown);
            loop {
                tokio::select! {
                    biased;
                    signal = &mut shutdown => { signal?; break; }
                    () = std::future::ready(()) => {}
                }
                // Finish the current transaction/outcome before honoring shutdown.
                // Canceling tick mid-flight could otherwise strand a claimed issue.
                match worker.tick().await? {
                    Some(issue) => println!("{} {:?}: {}", issue.id, issue.status, issue.title),
                    None if until_idle => break,
                    None => tokio::select! {
                        signal = &mut shutdown => { signal?; break; }
                        () = tokio::time::sleep(Duration::from_millis(500)) => {}
                    },
                }
            }
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
