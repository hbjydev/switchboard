use std::io::{IsTerminal, Write};

mod demo;

use clap::{Parser, Subcommand, ValueEnum};
use tracing::level_filters::LevelFilter;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

/// Switchboard is a persistent agentic AI gateway.
#[derive(Parser)]
struct Args {
    #[command(subcommand)]
    command: Command,

    /// The log format to use.
    #[clap(long, short = 'L', env = "SWITCHBOARD_LOG_FORMAT", global = true)]
    log_format: Option<LogFormat>,
}

#[derive(Subcommand)]
enum Command {
    /// Run a local human/agent conversation using an in-memory fake model.
    Demo {
        /// Human message to send to the agent.
        #[arg(long, default_value = "Hello, Switchboard!")]
        message: String,
    },
}

#[derive(Default, ValueEnum, Clone, Debug)]
enum LogFormat {
    #[clap(name = "compact")]
    #[default]
    Compact,

    #[clap(name = "pretty")]
    Pretty,

    #[clap(name = "json")]
    Json,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    init_tracing(&args.log_format.unwrap_or_default());

    match args.command {
        Command::Demo { message } => {
            let transcript = demo::run(message).await?;
            std::io::stdout().lock().write_all(transcript.as_bytes())?;
        }
    }

    Ok(())
}

fn init_tracing(format: &LogFormat) {
    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy();

    let fmt_layer: Box<dyn tracing_subscriber::Layer<tracing_subscriber::Registry> + Send + Sync> =
        match format {
            LogFormat::Compact => Box::new(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::io::stderr)
                    .with_file(false)
                    .with_line_number(false)
                    .with_ansi(std::io::stderr().is_terminal())
                    .pretty()
                    .compact(),
            ),

            LogFormat::Pretty => Box::new(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::io::stderr)
                    .with_file(false)
                    .with_line_number(false)
                    .with_ansi(std::io::stderr().is_terminal())
                    .pretty(),
            ),

            LogFormat::Json => Box::new(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::io::stderr)
                    .with_ansi(false)
                    .json(),
            ),
        };

    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(env_filter)
        .init();
}
