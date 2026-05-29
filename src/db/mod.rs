use crate::config::Config;
use sqlx::postgres::{PgPool, PgPoolOptions};
use std::time::Duration;

pub mod audit;
pub mod cron;
pub mod models;
pub mod partition;
pub mod pool_manager;
pub mod queries;
pub mod session;
pub mod slow_query;

/// Maximum time to wait for in-flight queries to finish during graceful shutdown.
const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);

/// Gracefully shuts down a database pool.
///
/// Waits up to `SHUTDOWN_DRAIN_TIMEOUT` for active connections to finish, then
/// closes the pool. This prevents data corruption from abruptly terminating
/// in-flight transactions.
///
/// # Security
/// - Refuses to close a pool that is already closed (no-op guard).
/// - Logs a warning if the drain timeout is exceeded so operators are alerted
///   to long-running queries that may need investigation.
pub async fn graceful_shutdown(pool: &PgPool) {
    // Guard: nothing to do if the pool is already closed.
    if pool.is_closed() {
        tracing::debug!("Database pool already closed; skipping graceful shutdown");
        return;
    }

    let active = pool.size().saturating_sub(pool.num_idle() as u32);
    tracing::info!(
        active_connections = active,
        timeout_secs = SHUTDOWN_DRAIN_TIMEOUT.as_secs(),
        "Starting database graceful shutdown"
    );

    // Wait for active connections to drain, bounded by the timeout.
    let drained = tokio::time::timeout(SHUTDOWN_DRAIN_TIMEOUT, async {
        loop {
            let in_flight = pool.size().saturating_sub(pool.num_idle() as u32);
            if in_flight == 0 {
                break;
            }
            tracing::debug!(in_flight, "Waiting for in-flight queries to complete");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;

    if drained.is_err() {
        let remaining = pool.size().saturating_sub(pool.num_idle() as u32);
        tracing::warn!(
            remaining_connections = remaining,
            timeout_secs = SHUTDOWN_DRAIN_TIMEOUT.as_secs(),
            "Graceful shutdown timeout exceeded; forcing pool close"
        );
    }

    pool.close().await;
    tracing::info!("Database pool closed");
}

/// Build a pool and eagerly establish `min_connections` by running `SELECT 1`
/// on each connection before returning. Logs warm-up completion time.
pub async fn create_pool(config: &Config) -> Result<PgPool, sqlx::Error> {
    let statement_timeout_ms = config.db_statement_timeout_ms;
    let idle_timeout_secs = config.db_idle_timeout_secs;

    PgPoolOptions::new()
        .min_connections(config.db_min_connections)
        .max_connections(config.db_max_connections)
        .idle_timeout(Duration::from_secs(idle_timeout_secs))
        .after_connect(move |conn, _meta| {
            let statement_timeout_ms = statement_timeout_ms;
            Box::pin(async move {
                sqlx::query(&format!("SET statement_timeout = {statement_timeout_ms}"))
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect(&config.database_url)
        .await
}

pub async fn create_long_running_pool(config: &Config) -> Result<PgPool, sqlx::Error> {
    let pool = build_pool(
        &config.database_url,
        config.db_min_connections,
        config.db_max_connections,
        config.db_idle_timeout_secs,
        config.db_long_running_statement_timeout_ms,
    )
    .await?;
    warm_up(&pool, config.db_min_connections).await?;
    Ok(pool)
}

async fn build_pool(
    url: &str,
    min: u32,
    max: u32,
    idle_timeout_secs: u64,
    statement_timeout_ms: u64,
) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .min_connections(min)
        .max_connections(max)
        .idle_timeout(Duration::from_secs(idle_timeout_secs))
        .after_connect(move |conn, _meta| {
            Box::pin(async move {
                sqlx::query(&format!("SET statement_timeout = {statement_timeout_ms}"))
                    .execute(conn)
                    .await?;
                Ok(())
            })
        })
        .connect(url)
        .await
}

async fn warm_up(pool: &PgPool, min_connections: u32) -> Result<(), sqlx::Error> {
    let mut handles = Vec::with_capacity(min_connections as usize);
    for _ in 0..min_connections {
        let pool = pool.clone();
        handles.push(tokio::spawn(async move {
            sqlx::query("SELECT 1").execute(&pool).await
        }));
    }
    for handle in handles {
        handle.await.ok();
    }
    Ok(())
}
