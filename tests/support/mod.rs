//! Shared fixture compiled only into integration test binaries.
use ledger::Ledger;
use sqlx::{PgPool, postgres::PgConnectOptions};
use testcontainers_modules::{
    postgres::Postgres,
    testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner},
};

/// Each test owns a PostgreSQL instance on a dynamically allocated host port.
/// Keep this guard alive until the test finishes; dropping it removes the container.
pub struct TestDatabase {
    pub pool: PgPool,
    _container: ContainerAsync<Postgres>,
}

impl TestDatabase {
    pub async fn start() -> Result<Self, Box<dyn std::error::Error>> {
        let container = Postgres::default()
            .with_fsync_enabled()
            .with_tag("17")
            .start()
            .await?;
        let options = PgConnectOptions::new()
            .host(&container.get_host().await?.to_string())
            .port(container.get_host_port_ipv4(5432).await?)
            .username("postgres")
            .password("postgres")
            .database("postgres");
        let pool = PgPool::connect_with(options).await?;
        Ledger::from_pool(pool.clone()).migrate().await?;
        Ok(Self {
            pool,
            _container: container,
        })
    }
}
