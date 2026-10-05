use std::{future::IntoFuture, net::SocketAddr, time::Duration};

use anyhow::{Context, Result};
use clap::Args;
use control_plane::{
    Application,
    auth::{AuthConfig, Authenticator},
    http::{ApiConfig, StatusResponse},
    scheduler::{SchedulerConfig, run},
};
use ledger::{Ledger, PeerKind};
use runtime::{Agent, CancellationToken, Worker};

use crate::{BackendKind, ExecutionOptions, SelectedBackend};

#[derive(Args)]
pub struct ServeOptions {
    #[arg(
        long,
        env = "SWITCHBOARD_LISTEN_ADDRESS",
        default_value = "0.0.0.0:8080"
    )]
    listen_address: SocketAddr,
    /// Disable autonomous execution for an API-only server.
    #[arg(long, env = "SWITCHBOARD_SCHEDULER", default_value_t = true, action = clap::ArgAction::Set)]
    scheduler: bool,
    #[arg(long, env = "SWITCHBOARD_WORKER_CONCURRENCY", default_value_t = 1, value_parser = clap::value_parser!(u16).range(1..))]
    worker_concurrency: u16,
    #[arg(long, env = "SWITCHBOARD_SCHEDULER_INTERVAL_MS", default_value_t = 1000, value_parser = clap::value_parser!(u64).range(1..))]
    scheduler_interval_ms: u64,
    /// Maximum time to wait for HTTP draining and execution cleanup.
    #[arg(long, env = "SWITCHBOARD_SHUTDOWN_GRACE_SECONDS", default_value_t = 10, value_parser = clap::value_parser!(u64).range(1..))]
    shutdown_grace_seconds: u64,
    #[arg(long, env = "SWITCHBOARD_OIDC_ISSUER")]
    oidc_issuer: String,
    #[arg(long, env = "SWITCHBOARD_OIDC_USER_AUDIENCE")]
    oidc_user_audience: String,
    #[arg(long, env = "SWITCHBOARD_OIDC_WORKLOAD_AUDIENCE")]
    oidc_workload_audience: Option<String>,
    #[command(flatten)]
    execution: ExecutionOptions,
}

impl ServeOptions {
    pub fn selected_backend(&self) -> Result<Option<SelectedBackend>> {
        if self.scheduler {
            Ok(Some(crate::selected_backend(&self.execution)?))
        } else {
            Ok(None)
        }
    }

    fn scheduler_config(&self) -> SchedulerConfig {
        SchedulerConfig {
            concurrency: usize::from(self.worker_concurrency),
            interval: Duration::from_millis(self.scheduler_interval_ms),
            shutdown_grace: Duration::from_secs(self.shutdown_grace_seconds),
        }
    }

    fn status(&self) -> StatusResponse {
        StatusResponse {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            scheduler_enabled: self.scheduler,
            scheduler_concurrency: usize::from(self.worker_concurrency),
            backend_kind: if self.scheduler {
                match self.execution.backend {
                    BackendKind::Fake => "fake",
                    BackendKind::Pi => "pi-local",
                }
            } else {
                "disabled"
            }
            .to_owned(),
        }
    }
}

pub async fn serve(
    ledger: Ledger,
    options: ServeOptions,
    backend: Option<SelectedBackend>,
    allow_loopback_http: bool,
) -> Result<()> {
    let app = Application::new(ledger.clone());
    app.check_ready().await.context(
        "database is unavailable or schema is incompatible; run switchboard migrate before serve",
    )?;
    let config = options.scheduler_config();
    config.validate()?;
    let auth = Authenticator::discover(AuthConfig {
        issuer: options.oidc_issuer.clone(),
        user_audience: options.oidc_user_audience.clone(),
        workload_audience: options.oidc_workload_audience.clone(),
        allow_loopback_http,
    })
    .await
    .context("could not initialize OIDC authentication")?;
    let api = control_plane::http::router(
        app,
        ApiConfig {
            auth,
            status: options.status(),
        },
    );
    let listener = tokio::net::TcpListener::bind(options.listen_address).await?;
    tracing::info!(address = %listener.local_addr()?, scheduler = options.scheduler, "control plane listening");
    let shutdown = CancellationToken::new();
    let worker = if let Some(backend) = backend {
        let peer = ledger
            .ensure_peer(&options.execution.agent, PeerKind::Agent)
            .await?;
        let worker = Worker::new(ledger, Agent { peer_id: peer.id }, backend)
            .with_execution_settings(options.execution.instructions, options.execution.workspace);
        Some(worker)
    } else {
        None
    };
    let signal = tokio::spawn(shutdown_signal(shutdown.clone()));
    let scheduler = worker.map(|worker| {
        let scheduler_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let result = run(worker, config, scheduler_shutdown.clone()).await;
            // A scheduler task failure terminates the service so orchestration
            // cannot silently disappear while HTTP continues serving.
            scheduler_shutdown.cancel();
            result
        })
    });
    let result = run_http(
        listener,
        api,
        shutdown.clone(),
        Duration::from_secs(options.shutdown_grace_seconds),
    )
    .await;
    shutdown.cancel();
    signal.abort();
    if let Some(scheduler) = scheduler {
        scheduler
            .await
            .context("scheduler task stopped unexpectedly")??;
    }
    result
}

async fn run_http(
    listener: tokio::net::TcpListener,
    api: axum::Router,
    shutdown: CancellationToken,
    grace: Duration,
) -> Result<()> {
    let http = axum::serve(listener, api)
        .with_graceful_shutdown(shutdown.clone().cancelled_owned())
        .into_future();
    tokio::pin!(http);
    tokio::select! {
        result = &mut http => result.map_err(anyhow::Error::from),
        () = shutdown.cancelled() => {
            tokio::time::timeout(grace, &mut http)
                .await.map_or_else(|_elapsed| { tracing::warn!("HTTP shutdown grace elapsed"); Ok(()) }, |result| result.map_err(anyhow::Error::from))
        }
    }
}

async fn shutdown_signal(shutdown: CancellationToken) {
    if let Err(error) = wait_for_signal().await {
        tracing::error!(%error, "shutdown signal handler failed");
    }
    shutdown.cancel();
}

async fn wait_for_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}
