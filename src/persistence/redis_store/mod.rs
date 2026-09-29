//! Redis-backed persistence: idempotency backend + event bus.
//!
//! ## Connection model
//!
//! - **Single-node**: `MultiplexedConnection` (cloneable, async-safe,
//!   handles pipelining internally). One underlying connection multiplexed
//!   across all clones.
//! - **Cluster**: `Arc<tokio::sync::Mutex<ClusterConnection>>`. The mutex
//!   serializes cluster ops; for higher throughput, run multiple workers
//!   each with its own connection.

pub mod event_bus;
pub mod idempotency;

pub use event_bus::{EventBusResult, RedisEventBus};
pub use idempotency::RedisIdempotencyBackend;

use crate::settings::RedisSettings;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

use redis::cluster::ClusterClient;
use redis::cluster_async::ClusterConnection;

/// Wrapper around the chosen Redis connection type.
#[derive(Clone)]
pub enum RedisConn {
    /// Single-node multiplexed connection (cloneable, async-safe).
    Single(redis::aio::MultiplexedConnection),
    /// Cluster connection, mutex-guarded (one in-flight op per mutex).
    Cluster(Arc<Mutex<ClusterConnection>>),
}

/// Build a Redis connection from settings.
pub async fn connect(settings: &RedisSettings) -> Result<RedisConn, redis::RedisError> {
    let _timeout = Duration::from_secs(settings.connect_timeout_secs);
    if settings.cluster {
        // For cluster, parse each URL into a ConnectionInfo.
        let urls: Vec<String> = settings
            .url
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect();
        let client = ClusterClient::new(urls)?;
        let conn = client.get_async_connection().await?;
        Ok(RedisConn::Cluster(Arc::new(Mutex::new(conn))))
    } else {
        let client = redis::Client::open(settings.url.as_str())?;
        let mut conn = client.get_multiplexed_async_connection().await?;
        // Quick PING to fail fast on unreachable Redis.
        let _ = redis::cmd("PING").query_async::<String>(&mut conn).await;
        Ok(RedisConn::Single(conn))
    }
}
