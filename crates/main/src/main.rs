use anyhow::Context;
use std::io::{IsTerminal, Write};
use std::{sync::Arc, time::Duration};
use switchboard_agent::ModelRef;
use switchboard_infrastructure::openai::{OpenAiConfig, OpenAiModel};

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
    /// Run a one-shot conversation through the `OpenAI` Responses API.
    Openai {
        #[arg(long, env = "OPENAI_MODEL")]
        model: String,
        #[arg(long, default_value = "Hello, Switchboard!")]
        message: String,
        #[arg(long, env = "OPENAI_TIMEOUT_SECONDS", default_value_t = 60)]
        timeout_seconds: u64,
        /// API prefix; HTTPS or loopback HTTP. Defaults to `OpenAI`'s /v1 endpoint.
        #[arg(long, env = "OPENAI_BASE_URL")]
        base_url: Option<String>,
        #[arg(long, default_value = "Respond helpfully to the conversation.")]
        instructions: String,
    },
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
        Command::Openai {
            model,
            message,
            timeout_seconds,
            base_url,
            instructions,
        } => {
            let transcript =
                run_openai(model, message, timeout_seconds, base_url, instructions).await?;
            std::io::stdout().lock().write_all(transcript.as_bytes())?;
        }
        Command::Demo { message } => {
            let transcript = demo::run(message).await?;
            std::io::stdout().lock().write_all(transcript.as_bytes())?;
        }
    }

    Ok(())
}

async fn run_openai(
    model: String,
    message: String,
    timeout_seconds: u64,
    base_url: Option<String>,
    instructions: String,
) -> anyhow::Result<String> {
    anyhow::ensure!(!model.trim().is_empty(), "OpenAI model must be nonblank");
    let api_key = std::env::var("OPENAI_API_KEY").context("OPENAI_API_KEY is required")?;
    let mut config = OpenAiConfig::new(&api_key, Duration::from_secs(timeout_seconds))?;
    if let Some(base_url) = base_url {
        config = config.with_base_url(&base_url)?;
    }
    let adapter = Arc::new(OpenAiModel::new(config)?);
    let cancellation = adapter.cancellation_token();
    let conversation = demo::run_with_model(
        message,
        adapter,
        ModelRef {
            provider: "openai".into(),
            model,
        },
        instructions,
    );
    tokio::pin!(conversation);
    tokio::select! {
        result = &mut conversation => result,
        signal = tokio::signal::ctrl_c() => {
            signal.context("registering Ctrl-C handler")?;
            cancellation.cancel();
            conversation.await
        }
    }
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
