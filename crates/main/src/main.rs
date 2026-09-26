use std::{io::IsTerminal, net::SocketAddr};

use clap::{Parser, ValueEnum};
use tracing::level_filters::LevelFilter;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

/// Switchboard is a persistent agentic AI gateway.
#[derive(Parser)]
struct Args {
    /// The address to bind the server to.
    #[clap(
        long,
        short,
        env = "SWITCHBOARD_BIND_ADDR",
        default_value = "0.0.0.0:8080"
    )]
    bind_addr: SocketAddr,

    /// The log format to use.
    #[clap(long, short = 'L', env = "SWITCHBOARD_LOG_FORMAT")]
    log_format: Option<LogFormat>,
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
async fn main() -> Result<(), String> {
    let args = Args::parse();
    init_tracing(&args.log_format.unwrap_or_default());

    tracing::info!("Hello, world!");

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
                    .with_file(false)
                    .with_line_number(false)
                    .with_ansi(std::io::stderr().is_terminal())
                    .pretty()
                    .compact(),
            ),

            LogFormat::Pretty => Box::new(
                tracing_subscriber::fmt::layer()
                    .with_file(false)
                    .with_line_number(false)
                    .with_ansi(std::io::stderr().is_terminal())
                    .pretty(),
            ),

            LogFormat::Json => Box::new(tracing_subscriber::fmt::layer().with_ansi(false).json()),
        };

    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(env_filter)
        .init();
}
