//! Redis-backed persistence: idempotency backend + event bus.
//!
//! ## Connection model
//!
//! - **Single-node**: `MultiplexedConnection` (cloneable, async-safe,
//!   handles pipelining internally). One underlying connection multiplexed
//!   across all clones.
//! - **Cluster**: `bb8::Pool<bb8_redis::RedisConnectionManager>`. The pool
//!   maintains N concurrent connections (configurable via
//!   `Settings::redis::pool_size`), so cluster ops run in parallel
//!   without serializing through a Mutex. This is the recommended
//!   production setup for high-throughput workers.
//!
//! ## Why bb8 instead of `Mutex<ClusterConnection>`
//!
//! The previous `tokio::sync::Mutex<ClusterConnection>` model
//! serialized all cluster ops through a single connection — only one
//! in-flight command per worker process. With bb8, the pool hands out
//! a fresh (multiplexed) connection per `get()` call, so concurrent
//! tasks can issue Redis commands in parallel. For a 16-concurrency
//! worker, this is roughly a 16x throughput improvement on the
//! Redis side.

pub mod event_bus;
pub mod idempotency;

pub use event_bus::{EventBusResult, RedisEventBus};
pub use idempotency::RedisIdempotencyBackend;

use crate::settings::RedisSettings;
use std::time::Duration;

use bb8::Pool;
use bb8_redis::RedisConnectionManager;

/// Wrapper around the chosen Redis connection type.
///
/// ## Single-node split
///
/// Blocking consumer operations (`XREADGROUP`, `XAUTOCLAIM`) and
/// producer operations (`XADD`, `XACK`, `XGROUP`) run on separate
/// `MultiplexedConnection`s. Redis processes commands from one
/// connection sequentially; without this split, a convoy of blocked
/// consumers would stall all produces/acks for up to
/// `concurrency × block_ms`.
#[derive(Clone)]
pub enum RedisConn {
    /// Single-node with separate consumer/producer connections.
    Single {
        /// Used for blocking `XREADGROUP` and `XAUTOCLAIM`.
        consumer: redis::aio::MultiplexedConnection,
        /// Used for `XADD`, `XACK`, and `XGROUP`.
        producer: redis::aio::MultiplexedConnection,
    },
    /// Cluster pool — N concurrent multiplexed connections, managed by
    /// `bb8`. Cloning the pool is cheap (it's `Arc` internally), so
    /// multiple worker tasks share the same underlying connections.
    Cluster(Pool<RedisConnectionManager>),
}

/// Build a Redis connection from settings.
pub async fn connect(settings: &RedisSettings) -> Result<RedisConn, redis::RedisError> {
    let _timeout = Duration::from_secs(settings.connect_timeout_secs);
    if settings.cluster {
        // For cluster mode, use bb8_redis::RedisConnectionManager +
        // the first URL (the manager handles slot routing internally;
        // it only needs one seed URL to discover the rest of the
        // cluster via CLUSTER NODES / CLUSTER SLOTS).
        let first_url = settings
            .url
            .split(',')
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(&settings.url);
        let manager = RedisConnectionManager::new(first_url)?;
        let pool = Pool::builder()
            .max_size(settings.pool_size)
            .build(manager)
            .await?;
        Ok(RedisConn::Cluster(pool))
    } else {
        let client = redis::Client::open(settings.url.as_str())?;
        let mut consumer = client.get_multiplexed_async_connection().await?;
        let mut producer = client.get_multiplexed_async_connection().await?;
        // Quick PINGs to fail fast on unreachable Redis.
        let _ = redis::cmd("PING")
            .query_async::<String>(&mut consumer)
            .await;
        let _ = redis::cmd("PING")
            .query_async::<String>(&mut producer)
            .await;
        Ok(RedisConn::Single { consumer, producer })
    }
}

/// Convert a bb8 `RunError<RedisError>` into a `redis::RedisError` so
/// that the existing `RedisResult`-returning methods can stay generic
/// across the `Single` and `Cluster` variants.
pub(crate) fn io_error_to_redis(e: bb8::RunError<redis::RedisError>) -> redis::RedisError {
    match e {
        bb8::RunError::User(e) => e,
        bb8::RunError::TimedOut => {
            redis::RedisError::from((redis::ErrorKind::IoError, "bb8 pool acquire timed out"))
        }
    }
}
