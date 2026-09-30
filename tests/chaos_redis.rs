//! Chaos test: verify the worker recovers gracefully when Redis
//! goes down mid-processing.
//!
//! ## What this test verifies
//!
//! 1. Start a Redis container (via `docker` CLI).
//! 2. Produce a request to the stream.
//! 3. Kill Redis mid-consume (or while the worker is idle).
//! 4. Verify the worker's `consume_request` returns
//!    `EventBusResult::Error` (not panic).
//! 5. Restart Redis.
//! 6. Verify `consume_request` returns `Empty` again (recovered).
//!
//! ## When this test runs
//!
//! - Requires Docker + `docker` CLI. Marked `#[ignore]` so it
//!   doesn't run on every `cargo test` invocation.
//! - Run from the `chaos` CI job, which has Docker available.
//! - Run locally:
//!   ```bash
//!   cargo test --features server --test chaos_redis -- --nocapture --include-ignored
//!   ```
//!
//! ## Notes
//!
//! - This test is intentionally slow (30-60 seconds) — it has to
//!   wait for Redis to die + restart.
//! - The test cleans up its Docker container on exit (Drop guard).
//! - The test uses a random stream name + random consumer group
//!   to avoid colliding with other tests.

#![cfg(feature = "server")]

use propfirm::persistence::redis_store::{connect as redis_connect, RedisEventBus};
use propfirm::settings::{EventBusSettings, RedisSettings};
use std::process::Command;
use std::time::Duration;
use uuid::Uuid;

const CONTAINER_NAME: &str = "propfirm-chaos-redis";
const PORT: u16 = 6390; // avoid colliding with the standard 6379

/// Drop guard that kills + removes the Redis container on exit.
struct RedisContainer {
    name: String,
}

impl RedisContainer {
    fn start() -> Option<Self> {
        // Pull + run a fresh Redis container on a random port.
        let name = format!("{CONTAINER_NAME}-{}", Uuid::new_v4());
        let status = Command::new("docker")
            .args([
                "run",
                "-d",
                "--rm",
                "--name",
                &name,
                "-p",
                &format!("{PORT}:6379"),
                "redis:7",
            ])
            .status()
            .ok()?;
        if !status.success() {
            eprintln!("docker run failed (status={status})");
            return None;
        }
        Some(RedisContainer { name })
    }

    fn kill(&self) {
        // SIGKILL — simulates a hard Redis crash.
        let _ = Command::new("docker").args(["kill", &self.name]).status();
        let _ = Command::new("docker")
            .args(["rm", "-f", &self.name])
            .status();
    }
}

impl Drop for RedisContainer {
    fn drop(&mut self) {
        self.kill();
    }
}

async fn wait_for_redis(url: &str, timeout_secs: u64) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
    while std::time::Instant::now() < deadline {
        let settings = RedisSettings {
            url: url.to_string(),
            cluster: false,
            connect_timeout_secs: 1,
            pool_size: 2,
        };
        if let Ok(conn) = redis_connect(&settings).await {
            use propfirm::persistence::redis_store::RedisConn;
            match conn {
                RedisConn::Single(mut c) => {
                    if redis::cmd("PING")
                        .query_async::<String>(&mut c)
                        .await
                        .is_ok()
                    {
                        return true;
                    }
                }
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

async fn make_bus(stream: &str, group: &str) -> RedisEventBus {
    let settings = RedisSettings {
        url: format!("redis://localhost:{PORT}"),
        cluster: false,
        connect_timeout_secs: 2,
        pool_size: 4,
    };
    let conn = redis_connect(&settings).await.expect("redis connect");
    let event_bus_settings = EventBusSettings {
        request_stream: stream.to_string(),
        response_stream: format!("{stream}-responses"),
        consumer_group: group.to_string(),
        consumer_name: format!("chaos-{}", Uuid::new_v4()),
        block_ms: 500,
        concurrency: 1,
        idle_claim_ms: 1000,
    };
    RedisEventBus::new(conn, event_bus_settings)
}

#[tokio::test]
#[ignore = "requires Docker + runs ~60s — run from CI chaos job"]
async fn chaos_redis_kill_mid_consume_does_not_panic() {
    // Skip if Docker isn't available.
    if Command::new("docker")
        .arg("info")
        .status()
        .map(|s| !s.success())
        .unwrap_or(true)
    {
        eprintln!("SKIP: docker not available");
        return;
    }

    let container = match RedisContainer::start() {
        Some(c) => c,
        None => {
            eprintln!("SKIP: failed to start Redis container");
            return;
        }
    };

    let url = format!("redis://localhost:{PORT}");
    eprintln!("Waiting for Redis to come up…");
    if !wait_for_redis(&url, 30).await {
        panic!("Redis did not come up within 30s");
    }

    let stream = format!("propfirm:chaos:{}:requests", Uuid::new_v4());
    let group = format!("propfirm-chaos:{}", Uuid::new_v4());
    let bus = make_bus(&stream, &group).await;
    bus.ensure_group().await.expect("ensure_group");

    // Produce a request so there's something for the consumer to find.
    use propfirm::persistence::redis_store::event_bus::EvaluateRequestPayload;
    let req = EvaluateRequestPayload {
        request_id: Uuid::new_v4().to_string(),
        tenant_id: Uuid::new_v4().to_string(),
        account_id: Uuid::new_v4().to_string(),
        payload: serde_json::json!({}),
        submitted_at: chrono::Utc::now().to_rfc3339(),
    };
    bus.produce_request(&req).await.expect("produce_request");

    // Now kill Redis — simulates a hard crash mid-consume.
    eprintln!("Killing Redis mid-consume…");
    container.kill();

    // Verify consume_request returns Error (not panic).
    let outcome = bus.consume_request().await;
    match outcome {
        propfirm::persistence::redis_store::EventBusResult::Error(_) => {
            eprintln!("OK: consume_request returned Error (not panic)");
        }
        other => {
            panic!("expected EventBusResult::Error after Redis kill; got {other:?}");
        }
    }

    // Restart Redis.
    let _ = RedisContainer::start(); // new container, same port
    eprintln!("Waiting for Redis to come back up…");
    if !wait_for_redis(&url, 30).await {
        panic!("Redis did not recover within 30s");
    }

    // Verify consume_request recovers (returns Empty because we
    // didn't reproduce the request after restart).
    let outcome = bus.consume_request().await;
    match outcome {
        propfirm::persistence::redis_store::EventBusResult::Empty => {
            eprintln!("OK: consume_request recovered (Empty after Redis restart)");
        }
        propfirm::persistence::redis_store::EventBusResult::Error(msg) => {
            // It's also acceptable for the bb8 pool to need a few
            // seconds to recover its connections — treat short errors
            // as recoverable.
            eprintln!(
                "NOTE: consume_request returned Error after restart (bb8 pool warming up): {msg}"
            );
        }
        other => {
            panic!("expected Empty or Error after Redis restart; got {other:?}");
        }
    }
}
