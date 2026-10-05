//! Persistent orchestration using the runtime's existing fenced Worker.

use std::{sync::Arc, time::Duration};

use runtime::{AgentBackend, CancellationToken, Worker};
use tokio::task::JoinSet;

#[derive(Clone, Copy, Debug)]
pub struct SchedulerConfig {
    pub concurrency: usize,
    pub interval: Duration,
    pub shutdown_grace: Duration,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            concurrency: 1,
            interval: Duration::from_secs(1),
            shutdown_grace: Duration::from_secs(10),
        }
    }
}

impl SchedulerConfig {
    pub const fn validate(self) -> Result<(), SchedulerError> {
        if self.concurrency == 0 || self.interval.is_zero() || self.shutdown_grace.is_zero() {
            return Err(SchedulerError::InvalidConfiguration);
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    #[error("scheduler concurrency, interval, and shutdown grace must be nonzero")]
    InvalidConfiguration,
    #[error("a scheduler task failed")]
    Task(#[from] tokio::task::JoinError),
}

/// Run fixed concurrent lanes until shutdown.
///
/// Each lane owns one Worker tick at a time; PostgreSQL remains the claim and
/// fencing correctness boundary.
/// Cleanup is bounded even for a backend that ignores cancellation.
pub async fn run<E: AgentBackend + 'static>(
    worker: Worker<E>,
    config: SchedulerConfig,
    shutdown: CancellationToken,
) -> Result<(), SchedulerError> {
    config.validate()?;
    let worker = Arc::new(worker);
    let cancellation = shutdown.child_token();
    let mut lanes = JoinSet::new();
    for _ in 0..config.concurrency {
        lanes.spawn(run_lane(
            worker.clone(),
            config.interval,
            cancellation.child_token(),
        ));
    }
    let failure = tokio::select! {
        biased;
        () = shutdown.cancelled() => None,
        result = lanes.join_next() => result.and_then(Result::err),
    };
    cancellation.cancel();
    if tokio::time::timeout(config.shutdown_grace, drain(&mut lanes))
        .await
        .is_err()
    {
        tracing::warn!("scheduler cleanup deadline reached; aborting active ticks");
        lanes.abort_all();
        drain(&mut lanes).await;
    }
    failure.map_or(Ok(()), |error| Err(SchedulerError::Task(error)))
}

async fn run_lane<E: AgentBackend>(
    worker: Arc<Worker<E>>,
    interval: Duration,
    cancellation: CancellationToken,
) {
    while !cancellation.is_cancelled() {
        match worker.tick_with_cancellation(cancellation.clone()).await {
            Ok(Some(_)) => continue,
            Ok(None) => {}
            Err(ledger::Error::ExecutionLost(attempt_id)) => {
                tracing::debug!(%attempt_id, "execution authority ended");
            }
            Err(_) => {
                // Do not format SQLx errors: they may contain connection details
                // or SQL text. A later poll retries after transient DB outages.
                tracing::warn!("scheduler tick failed; retrying after polling interval");
            }
        }
        tokio::select! {
            biased;
            () = cancellation.cancelled() => break,
            () = tokio::time::sleep(interval) => {}
        }
    }
}

async fn drain(lanes: &mut JoinSet<()>) {
    while let Some(result) = lanes.join_next().await {
        if let Err(error) = result
            && !error.is_cancelled()
        {
            tracing::error!("scheduler task failed during shutdown");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SchedulerConfig, SchedulerError};

    #[test]
    fn scheduler_rejects_zero_configuration() {
        for config in [
            SchedulerConfig {
                concurrency: 0,
                ..SchedulerConfig::default()
            },
            SchedulerConfig {
                interval: std::time::Duration::ZERO,
                ..SchedulerConfig::default()
            },
            SchedulerConfig {
                shutdown_grace: std::time::Duration::ZERO,
                ..SchedulerConfig::default()
            },
        ] {
            assert!(matches!(
                config.validate(),
                Err(SchedulerError::InvalidConfiguration)
            ));
        }
    }
}
