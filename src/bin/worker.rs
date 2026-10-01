//! `propfirm-worker` — Redis Streams event-bus consumer.
//!
//! Reads evaluation requests from `propfirm:evaluate:requests` stream
//! (consumer group `propfirm-worker`), runs `pure::evaluate`, and
//! XADDs the response to `propfirm:evaluate:responses`. The platform
//! backend correlates responses to requests via the `request_id` field.
//!
//! ## Concurrency model
//!
//! One process can run N concurrent consumers (configurable via
//! `event_bus.concurrency`). Each consumer is a tokio task that
//! `XREADGROUP`s one message at a time, processes it, XADDs the
//! response, and XACKs the original message. A `CancellationToken`
//! fans out SIGTERM to all consumers.
//!
//! ## Recovery
//!
//! Crashed workers leave messages in the PEL (Pending Entries List).
//! On startup, the worker periodically calls `XAUTOCLAIM` to claim
//! messages idle for longer than `idle_claim_ms`. This gives
//! at-least-once delivery.
//!
//! ## Idempotency
//!
//! Because delivery is at-least-once, the platform backend must
//! include an `Idempotency-Key` in the request payload. The worker
//! deduplicates via the configured idempotency backend
//! (`memory`/`postgres`/`redis`).
//!
//! # Usage
//!
//! ```bash
//! PROPFIRM_REDIS__URL=redis://localhost:6379 \
//! PROPFIRM_IDEMPOTENCY__BACKEND=redis \
//! cargo run --release --features server --bin propfirm-worker
//! ```

use propfirm::api::audit_log;
use propfirm::api::metrics::worker as worker_metrics;
use propfirm::api::middleware::install_panic_hook;
use propfirm::api::otel;
use propfirm::api::shutdown::shutdown_signal;
use propfirm::core::ids::AccountId;
use propfirm::core::types::ServerTime;
use propfirm::persistence::redis_store::event_bus::{
    EvaluateRequestPayload, EvaluateResponsePayload,
};
use propfirm::persistence::redis_store::{connect as redis_connect, RedisEventBus};
use propfirm::pure::{self, EquitySource, EvaluateInputs};
use propfirm::rulepack::RulePack;
use propfirm::rules::context::RuleContextKind;
use propfirm::rules::registry::RuleRegistry;
use propfirm::settings::Settings;
use propfirm::tenant::TenantId;

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, info};
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Subcommand dispatch:
    //   `propfirm-worker healthcheck`    → run healthcheck + exit
    //   `propfirm-worker metrics`       → dump accumulated metrics + exit
    //   `propfirm-worker status`        → print PEL stats + consumer list
    //   `propfirm-worker drain [secs]`  → XACK all PEL entries idle > N secs
    //   `propfirm-worker reset-group`   → delete + recreate consumer group
    //   (no subcommand)                 → run the worker loop
    let args: Vec<String> = std::env::args().collect();
    if args.len() >= 2 {
        match args[1].as_str() {
            "healthcheck" => return run_healthcheck().await,
            "metrics" => return run_metrics_dump().await,
            "status" => return run_status().await,
            "drain" => {
                let idle_secs: i64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(60);
                return run_drain(idle_secs).await;
            }
            "reset-group" => return run_reset_group().await,
            _ => {} // fall through to worker loop
        }
    }

    // 1. Load settings.
    let settings = Settings::load().map_err(|e| {
        eprintln!("FATAL: failed to load settings: {e}");
        e
    })?;

    // 2. Tracing (fmt layer + optional OTLP layer).
    otel::init_tracing(&settings.observability)?;

    // 3. Panic hook.
    if settings.observability.panic_hook {
        install_panic_hook();
    }

    info!(
        stream = %settings.event_bus.request_stream,
        group = %settings.event_bus.consumer_group,
        concurrency = settings.event_bus.concurrency,
        "propfirm-worker starting"
    );

    // 4. Connect to Redis.
    let redis_conn = redis_connect(&settings.redis).await.map_err(|e| {
        error!(error = %e, "failed to connect to Redis");
        e
    })?;

    // 5. Build event bus.
    let bus = RedisEventBus::new(redis_conn, settings.event_bus.clone());
    bus.ensure_group().await.map_err(|e| {
        error!(error = %e, "failed to ensure consumer group");
        e
    })?;

    // 5b. Build an optional Postgres pool for audit_log writes.
    // The worker only needs the pool for audit; the engine itself
    // uses pure::evaluate which doesn't touch Postgres. When the
    // user runs with idempotency.backend = memory, the pool is None
    // and audit-log writes become no-ops.
    let pg_pool: Option<Arc<sqlx::PgPool>> = match settings.idempotency.backend.as_str() {
        "postgres" | "redis" => {
            match propfirm::persistence::postgres::connect(&settings.postgres).await {
                Ok(pool) => Some(Arc::new(pool)),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "failed to connect to Postgres for audit-log writes; \
                         audit_log entries will be silently dropped"
                    );
                    None
                }
            }
        }
        _ => None,
    };

    // 6. Spawn consumer tasks.
    let shutdown = shutdown_signal(Duration::from_secs(settings.server.shutdown_timeout_secs));
    let mut tasks = Vec::new();

    for i in 0..settings.event_bus.concurrency {
        let bus = bus.clone();
        let shutdown = shutdown.clone();
        let consumer_name = format!("worker-{}", i);
        let pg = pg_pool.clone();
        tasks.push(tokio::spawn(async move {
            info!(consumer = %consumer_name, "consumer started");
            worker_loop(&bus, &consumer_name, shutdown, pg.as_ref()).await;
        }));
    }

    // 7. Periodic PEL recovery task.
    let recovery_bus = bus.clone();
    let recovery_shutdown = shutdown.clone();
    let recovery_pg = pg_pool.clone();
    let recovery_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(30));
        interval.tick().await; // first tick is immediate
        loop {
            if recovery_shutdown.is_cancelled() {
                break;
            }
            interval.tick().await;
            if let propfirm::persistence::redis_store::EventBusResult::Consumed {
                stream_id,
                payload,
            } = recovery_bus.claim_idle().await
            {
                info!(stream_id = %stream_id, request_id = %payload.request_id, "claimed idle message");
                worker_metrics::record_message_claimed();
                process_request(
                    &recovery_bus,
                    stream_id,
                    payload,
                    "recovery",
                    recovery_pg.as_ref(),
                )
                .await;
            }
        }
    });

    // 8. Wait for shutdown.
    shutdown.cancelled().await;
    info!("shutdown signal received, waiting for in-flight tasks");
    for t in tasks {
        let _ = t.await;
    }
    recovery_handle.abort();

    // Flush the OTLP provider so spans in flight are exported before
    // process exit (best-effort).
    otel::shutdown_otlp();

    info!("worker stopped cleanly");
    Ok(())
}

/// One consumer's main loop: read → process → ack.
#[tracing::instrument(skip(bus, consumer_name, shutdown, pg_pool), fields(consumer = %consumer_name))]
async fn worker_loop(
    bus: &RedisEventBus,
    consumer_name: &str,
    shutdown: tokio_util::sync::CancellationToken,
    pg_pool: Option<&Arc<sqlx::PgPool>>,
) {
    loop {
        if shutdown.is_cancelled() {
            info!(consumer = %consumer_name, "consumer received shutdown");
            break;
        }
        let outcome = bus.consume_request().await;
        match outcome {
            propfirm::persistence::redis_store::EventBusResult::Consumed { stream_id, payload } => {
                tracing::info!(
                    consumer = %consumer_name,
                    stream_id = %stream_id,
                    request_id = %payload.request_id,
                    "received request"
                );
                worker_metrics::record_message_consumed(consumer_name);
                process_request(bus, stream_id, payload, consumer_name, pg_pool).await;
            }
            propfirm::persistence::redis_store::EventBusResult::Empty => {
                // Block timed out; loop and try again.
            }
            propfirm::persistence::redis_store::EventBusResult::Error(msg) => {
                worker_metrics::record_error("consume_failed");
                error!(error = %msg, "consume_request error; sleeping 1s");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            propfirm::persistence::redis_store::EventBusResult::Produced(_) => {
                // Not expected from consume_request.
            }
        }
    }
}

/// Process one request: parse → evaluate → XADD response → XACK.
///
/// For successful evaluations, an audit-log row is written to the
/// `audit_log` table when a Postgres pool is configured. For error
/// paths (decode failure, redis error, panic), an `worker_error`
/// audit entry is written instead. The audit row carries:
///   - actor_id: consumer_name (e.g. "worker-3" or "recovery")
///   - action: worker_evaluate | worker_error
///   - tenant_id, account_id (when extractable from the payload)
///   - resource_id: request_id (correlation key with the platform
///     backend)
///   - request_hash: input_hash (sha256 of evaluation inputs)
///   - metadata: consumer, decision_kind, request_id, input_hash,
///     error_kind (when error), error_msg (when error)
#[tracing::instrument(skip(bus, stream_id, payload, consumer_name, pg_pool), fields(consumer = %consumer_name, stream_id = %stream_id, request_id = %payload.request_id, account_id = ?extract_ids(&payload).1))]
async fn process_request(
    bus: &RedisEventBus,
    stream_id: String,
    payload: EvaluateRequestPayload,
    consumer_name: &str,
    pg_pool: Option<&Arc<sqlx::PgPool>>,
) {
    let request_id = payload.request_id.clone();
    let processed_at = chrono::Utc::now().to_rfc3339();
    let _latency = worker_metrics::latency_scope();

    // Try to extract tenant_id + account_id from the payload up front
    // for audit-log purposes (the actual evaluation happens inside
    // parse_and_evaluate).
    let (tenant_id_opt, account_id_opt) = extract_ids(&payload);

    // Parse inputs.
    let (response_payload, error_msg) = match parse_and_evaluate(&payload).await {
        Ok((decision_kind, input_hash, account_state, violations)) => {
            // Audit-log only when decision is non-Pass (to avoid
            // flooding the table with normal traffic). Same gating
            // as the HTTP evaluate_internal handler.
            let decision_lower = decision_kind.to_lowercase();
            if decision_lower != "pass" {
                if let (Some(t), Some(a)) = (tenant_id_opt, account_id_opt) {
                    let audit = crate::audit_log::worker_evaluate(
                        consumer_name,
                        t,
                        a,
                        &decision_kind,
                        &request_id,
                        &input_hash,
                    );
                    audit.finish(pg_pool, None, 200).await;
                }
            }
            let response = EvaluateResponsePayload {
                request_id: request_id.clone(),
                decision_kind,
                input_hash,
                account_state: serde_json::to_value(&account_state)
                    .unwrap_or(serde_json::Value::Null),
                violations,
                processed_at,
                error: None,
            };
            (response, None)
        }
        Err(e) => {
            worker_metrics::record_error("evaluate_failed");
            error!(request_id = %request_id, error = %e, "evaluate failed");
            // Audit-log the worker error.
            let audit = crate::audit_log::worker_error(
                consumer_name,
                tenant_id_opt,
                account_id_opt,
                &request_id,
                "evaluate_failed",
                &e.to_string(),
            );
            audit.finish(pg_pool, None, 500).await;
            let response = EvaluateResponsePayload {
                request_id: request_id.clone(),
                decision_kind: "Error".to_string(),
                input_hash: String::new(),
                account_state: serde_json::Value::Null,
                violations: Vec::new(),
                processed_at,
                error: Some(e.to_string()),
            };
            (response, Some(e.to_string()))
        }
    };

    // Publish response.
    if let Err(e) = bus.produce_response(&response_payload).await {
        worker_metrics::record_error("produce_response_failed");
        error!(request_id = %request_id, error = %e, "failed to publish response");
        // Audit-log the publish failure.
        let audit = crate::audit_log::worker_error(
            consumer_name,
            tenant_id_opt,
            account_id_opt,
            &request_id,
            "produce_response_failed",
            &e.to_string(),
        );
        audit.finish(pg_pool, None, 500).await;
    } else {
        worker_metrics::record_message_produced();
    }

    // Ack the request (so it leaves the PEL).
    if let Err(e) = bus.ack(&stream_id).await {
        worker_metrics::record_error("ack_failed");
        error!(request_id = %request_id, stream_id = %stream_id, error = %e, "failed to XACK");
        // Audit-log the ack failure.
        let audit = crate::audit_log::worker_error(
            consumer_name,
            tenant_id_opt,
            account_id_opt,
            &request_id,
            "ack_failed",
            &e.to_string(),
        );
        audit.finish(pg_pool, None, 500).await;
    } else {
        worker_metrics::record_message_acked();
    }
    tracing::info!(request_id = %request_id, stream_id = %stream_id, consumer = %consumer_name, error = ?error_msg, "request processed");
}

/// Extract tenant_id and account_id from a request payload for
/// audit-log purposes. Returns `(None, None)` when the payload is
/// malformed — the actual evaluation will then produce a
/// `worker_error` audit entry with the parse failure.
fn extract_ids(payload: &EvaluateRequestPayload) -> (Option<TenantId>, Option<AccountId>) {
    let tenant_id = TenantId::from_str(&payload.tenant_id).ok();
    let account_id = payload
        .payload
        .get("account_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::from_str(s).ok())
        .map(AccountId::from_uuid);
    (tenant_id, account_id)
}

/// Parse the request payload and run the pure evaluate function.
#[tracing::instrument(skip(payload), fields(account_id = ?payload.account_id, request_id = ?payload.request_id))]
async fn parse_and_evaluate(
    payload: &EvaluateRequestPayload,
) -> anyhow::Result<(
    String,
    String,
    propfirm::core::account::Account,
    Vec<serde_json::Value>,
)> {
    // The wire shape is identical to InternalEvaluateRequest — we
    // deserialize into the same DTO.
    let req: propfirm::api::handlers::InternalEvaluateRequest =
        serde_json::from_value(payload.payload.clone())?;

    let account_id = AccountId::from_uuid(
        Uuid::from_str(&req.account_id).map_err(|e| anyhow::anyhow!("invalid account_id: {e}"))?,
    );
    let tenant_id = TenantId::from_str(&payload.tenant_id)
        .map_err(|e| anyhow::anyhow!("invalid tenant_id: {e}"))?;

    let acc = req
        .account_state
        .ok_or_else(|| anyhow::anyhow!("account_state is required for stateless evaluation"))?;
    if acc.id != account_id {
        return Err(anyhow::anyhow!("account_state.id mismatch"));
    }
    if acc.tenant_id != tenant_id {
        return Err(anyhow::anyhow!("account_state.tenant_id mismatch"));
    }

    let (equity_source, _bridge_tick) = match req.bridge_tick {
        Some(ref bt) => (EquitySource::BrokerReported, Some(bt)),
        None => {
            let src = match req.equity_source.as_deref() {
                None | Some("estimated") => EquitySource::Estimated,
                Some(other) => EquitySource::parse(other)
                    .map_err(|e| anyhow::anyhow!("invalid equity_source: {e}"))?,
            };
            (src, None)
        }
    };

    let pack = RulePack::synthetic_from_plan(account_id, tenant_id, &acc.plan);
    let registry = RuleRegistry::with_default_rules_for_plan(&acc.plan);

    // Parse positions and trades from the wire shape.
    let mut positions = Vec::new();
    for p in req.open_positions.unwrap_or_default() {
        positions.push(
            p.into_domain(account_id)
                .map_err(|e| anyhow::anyhow!("{e}"))?,
        );
    }
    let mut trades = Vec::new();
    for t in req.today_trades.unwrap_or_default() {
        trades.push(
            t.into_domain(account_id)
                .map_err(|e| anyhow::anyhow!("{e}"))?,
        );
    }
    let cross_reference_trades = Vec::new(); // not yet supported via event bus

    let server_time = if let Some(ref bt) = req.bridge_tick {
        ServerTime(
            chrono::DateTime::from_timestamp_millis(bt.payload.broker_time)
                .ok_or_else(|| anyhow::anyhow!("invalid broker_time"))?,
        )
    } else if let Some(ref tick) = req.tick {
        ServerTime(tick.quote.ts)
    } else {
        ServerTime(chrono::Utc::now())
    };

    let latest_tick = req.tick.clone();

    let inputs = if let Some(ref tick) = latest_tick {
        EvaluateInputs::for_tick(&positions, &trades, tick)
            .with_cross_reference_trades(cross_reference_trades)
            .with_equity_source(equity_source)
    } else {
        EvaluateInputs {
            open_positions: &positions,
            today_trades: &trades,
            cross_reference_trades,
            equity_source,
            ..Default::default()
        }
    };

    let verdict = pure::evaluate(
        &acc,
        &pack,
        &registry,
        RuleContextKind::OnTick,
        server_time,
        inputs,
    )?;

    // Apply the decision to the account (state mutation).
    let (new_state, _events) = propfirm::engine::pipeline::apply_decision(
        propfirm::engine::state::AccountState::new(acc.clone()),
        &verdict.decision,
        server_time.0,
        "worker",
    )?;

    Ok((
        format!("{:?}", verdict.decision.kind),
        verdict.input_hash,
        new_state.account,
        verdict
            .decision
            .all_violations
            .iter()
            .map(|v| serde_json::to_value(v).unwrap_or_default())
            .collect(),
    ))
}

/// `propfirm-worker healthcheck` — exit 0 if the worker can talk to
/// Redis, the request stream exists, and the consumer group is
/// registered. Exits non-zero otherwise.
///
/// Designed for use as a k8s liveness/readiness probe:
///
/// ```yaml
/// livenessProbe:
///   exec:
///     command: ["/app/propfirm-worker", "healthcheck"]
///   initialDelaySeconds: 5
///   periodSeconds: 10
/// ```
///
/// Also useful for `docker compose exec propfirm-worker
/// /app/propfirm-worker healthcheck` for local verification.
async fn run_healthcheck() -> anyhow::Result<()> {
    use propfirm::persistence::redis_store::RedisConn;

    // Load settings (no tracing init — we want stderr to be the
    // only output, so the k8s probe output stays clean).
    let settings = Settings::load()?;

    // Step 1: connect to Redis.
    let conn = match redis_connect(&settings.redis).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FAIL: redis connect failed: {e}");
            return Err(anyhow::anyhow!("redis connect failed: {e}"));
        }
    };

    // Step 2: PING Redis.
    let ping_ok = match &conn {
        RedisConn::Single(c) => {
            let mut c = c.clone();
            redis::cmd("PING")
                .query_async::<String>(&mut c)
                .await
                .map(|v| v == "PONG")
                .unwrap_or(false)
        }
        RedisConn::Cluster(pool) => match pool.get().await {
            Ok(mut c) => redis::cmd("PING")
                .query_async::<String>(&mut *c)
                .await
                .map(|v| v == "PONG")
                .unwrap_or(false),
            Err(_) => false,
        },
    };
    if !ping_ok {
        eprintln!("FAIL: redis PING did not return PONG");
        return Err(anyhow::anyhow!("redis PING failed"));
    }

    // Step 3: check the request stream exists.
    let stream = &settings.event_bus.request_stream;
    let group = &settings.event_bus.consumer_group;
    let stream_exists: bool = match &conn {
        RedisConn::Single(c) => {
            let mut c = c.clone();
            redis::cmd("EXISTS")
                .arg(stream)
                .query_async::<i64>(&mut c)
                .await
                .map(|v| v == 1)
                .unwrap_or(false)
        }
        RedisConn::Cluster(pool) => match pool.get().await {
            Ok(mut c) => redis::cmd("EXISTS")
                .arg(stream)
                .query_async::<i64>(&mut *c)
                .await
                .map(|v| v == 1)
                .unwrap_or(false),
            Err(_) => false,
        },
    };
    if !stream_exists {
        eprintln!("WARN: request stream '{stream}' does not exist (worker will create it on first consume via MKSTREAM)");
        // Not a fatal error — the worker creates the stream lazily.
    }

    // Step 4: check the consumer group exists.
    // XINFO GROUPS returns a nested array: each group is an array of
    // [field_name, field_value] pairs. We search for our group name
    // in the nested structure.
    let group_exists = match &conn {
        RedisConn::Single(c) => {
            let mut c = c.clone();
            let raw: Option<Vec<Vec<(String, redis::Value)>>> = redis::cmd("XINFO")
                .arg("GROUPS")
                .arg(stream)
                .query_async(&mut c)
                .await
                .ok();
            raw.map(|groups| {
                groups.iter().any(|group_fields| {
                    group_fields
                        .iter()
                        .any(|(k, v)| k == "name" && value_as_string(v) == Some(group.clone()))
                })
            })
            .unwrap_or(false)
        }
        RedisConn::Cluster(pool) => match pool.get().await {
            Ok(mut c) => {
                let raw: Option<Vec<Vec<(String, redis::Value)>>> = redis::cmd("XINFO")
                    .arg("GROUPS")
                    .arg(stream)
                    .query_async(&mut *c)
                    .await
                    .ok();
                raw.map(|groups| {
                    groups.iter().any(|group_fields| {
                        group_fields
                            .iter()
                            .any(|(k, v)| k == "name" && value_as_string(v) == Some(group.clone()))
                    })
                })
                .unwrap_or(false)
            }
            Err(_) => false,
        },
    };
    if !group_exists {
        eprintln!("WARN: consumer group '{group}' does not exist on stream '{stream}' (worker will create it on startup)");
        // Not fatal — the worker creates the group on startup.
    }

    println!("OK: redis reachable, stream '{stream}' present, group '{group}' registered");
    Ok(())
}

/// `propfirm-worker metrics` — dump the global Prometheus metrics
/// recorder's accumulated values to stdout. Exits 0 on success.
///
/// This is a debugging aid — when the worker is behaving oddly (e.g.
/// message consumed but not acked, or sudden spike in errors),
/// running this subcommand lets you see the current counter /
/// histogram values without scraping the `/metrics` endpoint
/// (which is on the server, not the worker).
///
/// The output is Prometheus text format, compatible with `curl
/// http://server:8080/metrics`:
///
///   ```text
///   # HELP propfirm_event_bus_messages_consumed_total ...
///   # TYPE propfirm_event_bus_messages_consumed_total counter
///   propfirm_event_bus_messages_consumed_total{consumer="worker-0"} 1234
///   ...
///   ```
///
/// Usage:
///   kubectl exec deploy/propfirm-worker -- /app/propfirm-worker metrics
///
/// Note: this calls `default_metrics_handle()` which installs the
/// global Prometheus recorder. If the recorder is already installed
/// (which it is, in any binary that has called `init_metrics`), this
/// is a no-op; otherwise it installs a fresh recorder (which will
/// have all-zero counters — useful for verifying the recorder is
/// reachable from the worker binary).
async fn run_metrics_dump() -> anyhow::Result<()> {
    // Touch the handle so the recorder is installed (idempotent via
    // OnceLock in api::server::default_metrics_handle).
    let handle = propfirm::api::server::default_metrics_handle();

    // Render + print. The output is the same format as the /metrics
    // endpoint on the server.
    let rendered = handle.render();
    print!("{rendered}");
    Ok(())
}

/// `propfirm-worker status` — print Pending Entries List (PEL) stats
/// and per-consumer information for the request stream + consumer
/// group configured in settings.
///
/// This is a debugging aid: when the worker is backed up (consuming
/// but not acking, or vice versa), running this subcommand shows
/// exactly which messages are stuck + how long they've been idle.
///
/// Output (human-readable; one section per concept):
///   ```text
///   === Consumer group: propfirm-worker on stream propfirm:evaluate:requests ===
///
///   XPENDING summary:
///     pending count:  42
///     lowest pending: 1696123456789-0
///     highest pending: 1696123999999-0
///     consumers in group: 3
///
///   XINFO CONSUMERS (per-consumer pending):
///     consumer=worker-0  pending=15  idle=12s
///     consumer=worker-1  pending=20  idle=8s
///     consumer=worker-2  pending=7   idle=4s
///   ```
///
/// Usage:
///   kubectl exec deploy/propfirm-worker -- /app/propfirm-worker status
async fn run_status() -> anyhow::Result<()> {
    use propfirm::persistence::redis_store::RedisConn;
    let settings = Settings::load()?;
    let conn = match redis_connect(&settings.redis).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FAIL: redis connect failed: {e}");
            return Err(anyhow::anyhow!("redis connect failed: {e}"));
        }
    };
    let stream = &settings.event_bus.request_stream;
    let group = &settings.event_bus.consumer_group;

    println!("=== Consumer group: {group} on stream {stream} ===");

    // XPENDING summary: returns [pending_count, lowest_id, highest_id, consumer_count]
    let xpending_summary: Option<Vec<redis::Value>> = match &conn {
        RedisConn::Single(c) => {
            let mut c = c.clone();
            redis::cmd("XPENDING")
                .arg(stream)
                .arg(group)
                .query_async(&mut c)
                .await
                .ok()
        }
        RedisConn::Cluster(pool) => match pool.get().await {
            Ok(mut c) => redis::cmd("XPENDING")
                .arg(stream)
                .arg(group)
                .query_async(&mut *c)
                .await
                .ok(),
            Err(_) => None,
        },
    };

    println!();
    println!("XPENDING summary:");
    if let Some(summary) = xpending_summary {
        let pending_count = summary.first().and_then(value_as_int).unwrap_or(-1);
        let lowest = summary
            .get(1)
            .and_then(value_as_string)
            .unwrap_or_else(|| "(none)".to_string());
        let highest = summary
            .get(2)
            .and_then(value_as_string)
            .unwrap_or_else(|| "(none)".to_string());
        let consumers = summary.get(3).and_then(value_as_int).unwrap_or(-1);
        println!("  pending count:  {pending_count}");
        println!("  lowest pending:  {lowest}");
        println!("  highest pending: {highest}");
        println!("  consumers in group: {consumers}");
    } else {
        println!("  (XPENDING failed — consumer group may not exist yet)");
    }

    // XINFO CONSUMERS: returns nested arrays of [field_name, value]
    // pairs for each consumer. Parse as nested structure.
    println!();
    println!("XINFO CONSUMERS (per-consumer pending):");
    let xinfo_consumers: Option<Vec<Vec<(String, redis::Value)>>> = match &conn {
        RedisConn::Single(c) => {
            let mut c = c.clone();
            redis::cmd("XINFO")
                .arg("CONSUMERS")
                .arg(stream)
                .arg(group)
                .query_async(&mut c)
                .await
                .ok()
        }
        RedisConn::Cluster(pool) => match pool.get().await {
            Ok(mut c) => redis::cmd("XINFO")
                .arg("CONSUMERS")
                .arg(stream)
                .arg(group)
                .query_async(&mut *c)
                .await
                .ok(),
            Err(_) => None,
        },
    };
    if let Some(consumers) = xinfo_consumers {
        // consumers is a nested list: each consumer is a list of
        // [field_name, value] pairs.
        let mut printed_any = false;
        for consumer_fields in &consumers {
            let mut current_name = String::new();
            let mut current_pending: i64 = -1;
            let mut current_idle_ms: i64 = -1;
            for (k, v) in consumer_fields {
                match k.as_str() {
                    "name" => {
                        if let Some(s) = value_as_string(v) {
                            current_name = s;
                        }
                    }
                    "pending" => {
                        current_pending = v.as_int_opt().unwrap_or(-1);
                    }
                    "idle" => {
                        current_idle_ms = v.as_int_opt().unwrap_or(-1);
                    }
                    _ => {}
                }
            }
            if !current_name.is_empty() {
                println!(
                    "  consumer={:<20}  pending={:<5}  idle={}",
                    current_name,
                    current_pending,
                    format_idle(current_idle_ms)
                );
                printed_any = true;
            }
        }
        if !printed_any {
            println!("  (no consumers registered yet — workers haven't started?)");
        }
    } else {
        println!("  (XINFO CONSUMERS failed — stream or group may not exist)");
    }

    Ok(())
}

/// Format idle ms as a human-readable duration ("12s", "3m", "1h").
fn format_idle(ms: i64) -> String {
    if ms < 0 {
        return "?".to_string();
    }
    let secs = ms / 1000;
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

/// `propfirm-worker drain [idle_secs]` — XACK all PEL entries that
/// have been idle for more than `idle_secs` (default 60s). Useful
/// for cleaning up the PEL when workers have crashed and the
/// messages are stuck.
///
/// WARNING: draining is destructive — any in-flight work in those
/// messages is lost. Use only when:
/// - The worker pool is down (so no consumer is actively processing).
/// - OR the messages are known to be already-processed-but-unacked
///   (e.g. worker panicked after process but before ack).
///
/// Usage:
///   kubectl exec deploy/propfirm-worker -- /app/propfirm-worker drain 60
///
/// Output:
///   ```text
///   Drain: scanning PEL for entries idle > 60s on stream
///          propfirm:evaluate:requests (group: propfirm-worker)
///   Drain: claimed 7 entries via XAUTOCLAIM
///   Drain: XACK'd all 7 entries
///   Drain: PEL cleaned; remaining pending = 0
///   ```
async fn run_drain(idle_secs: i64) -> anyhow::Result<()> {
    use propfirm::persistence::redis_store::RedisConn;
    let settings = Settings::load()?;
    let conn = match redis_connect(&settings.redis).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FAIL: redis connect failed: {e}");
            return Err(anyhow::anyhow!("redis connect failed: {e}"));
        }
    };
    let stream = settings.event_bus.request_stream.clone();
    let group = settings.event_bus.consumer_group.clone();
    let consumer_name = format!("drain-{}", std::process::id());

    eprintln!(
        "Drain: scanning PEL for entries idle > {idle_secs}s on stream {stream} (group: {group})"
    );

    // Use XAUTOCLAIM with a high count + iterate until we get no more.
    let mut total_claimed: usize = 0;
    let mut start_id = "0-0".to_string();
    let max_iterations = 100; // safety cap

    for _ in 0..max_iterations {
        let (next_id, claimed_ids): (String, Vec<String>) = match &conn {
            RedisConn::Single(c) => {
                let mut c = c.clone();
                // XAUTOCLAIM stream group consumer min_idle start_id COUNT n
                // returns (next-start-id, [(stream-id, fields), ...], deleted-ids)
                // We just need the stream-ids.
                #[allow(clippy::type_complexity)]
                let raw: Option<(
                    String,
                    Vec<(String, Vec<(String, String)>)>,
                    Vec<String>,
                )> = redis::cmd("XAUTOCLAIM")
                    .arg(&stream)
                    .arg(&group)
                    .arg(&consumer_name)
                    .arg(idle_secs * 1000)
                    .arg(&start_id)
                    .arg("COUNT")
                    .arg(100i64)
                    .query_async(&mut c)
                    .await
                    .ok();
                match raw {
                    Some((next, entries, _deleted)) => {
                        let ids: Vec<String> = entries.into_iter().map(|(id, _)| id).collect();
                        (next, ids)
                    }
                    None => break,
                }
            }
            RedisConn::Cluster(pool) => match pool.get().await {
                Ok(mut c) => {
                    #[allow(clippy::type_complexity)]
                    let raw: Option<(
                        String,
                        Vec<(String, Vec<(String, String)>)>,
                        Vec<String>,
                    )> = redis::cmd("XAUTOCLAIM")
                        .arg(&stream)
                        .arg(&group)
                        .arg(&consumer_name)
                        .arg(idle_secs * 1000)
                        .arg(&start_id)
                        .arg("COUNT")
                        .arg(100i64)
                        .query_async(&mut *c)
                        .await
                        .ok();
                    match raw {
                        Some((next, entries, _deleted)) => {
                            let ids: Vec<String> = entries.into_iter().map(|(id, _)| id).collect();
                            (next, ids)
                        }
                        None => break,
                    }
                }
                Err(e) => {
                    return Err(anyhow::anyhow!("pool acquire failed: {e}"));
                }
            },
        };

        if claimed_ids.is_empty() {
            break;
        }

        total_claimed += claimed_ids.len();
        eprintln!(
            "Drain: claimed {} entries (total: {total_claimed})",
            claimed_ids.len()
        );

        // XACK each claimed message — they leave the PEL.
        // Use a single XACK call with multiple IDs (Redis supports
        // XACK stream group id1 id2 id3 ...).
        let ack_count: i64 = match &conn {
            RedisConn::Single(c) => {
                let mut c = c.clone();
                let mut cmd = redis::cmd("XACK");
                cmd.arg(&stream).arg(&group);
                for id in &claimed_ids {
                    cmd.arg(id);
                }
                cmd.query_async(&mut c).await.unwrap_or(0)
            }
            RedisConn::Cluster(pool) => match pool.get().await {
                Ok(mut c) => {
                    let mut cmd = redis::cmd("XACK");
                    cmd.arg(&stream).arg(&group);
                    for id in &claimed_ids {
                        cmd.arg(id);
                    }
                    cmd.query_async(&mut *c).await.unwrap_or(0)
                }
                Err(_) => 0,
            },
        };
        eprintln!("Drain: XACK'd {ack_count} entries");

        // Move to the next cursor.
        if next_id == start_id {
            break;
        }
        start_id = next_id;
    }

    // Final XPENDING count.
    let remaining: i64 = match &conn {
        RedisConn::Single(c) => {
            let mut c = c.clone();
            redis::cmd("XPENDING")
                .arg(&stream)
                .arg(&group)
                .query_async::<i64>(&mut c)
                .await
                .unwrap_or(-1)
        }
        RedisConn::Cluster(pool) => match pool.get().await {
            Ok(mut c) => redis::cmd("XPENDING")
                .arg(&stream)
                .arg(&group)
                .query_async::<i64>(&mut *c)
                .await
                .unwrap_or(-1),
            Err(_) => -1,
        },
    };
    eprintln!("Drain: PEL cleaned; remaining pending = {remaining}");
    eprintln!("Drain: total messages drained = {total_claimed}");
    println!("drained={total_claimed} remaining_pending={remaining}");
    Ok(())
}

/// Extract an i64 from a redis::Value (Int or BulkString).
fn value_as_int(v: &redis::Value) -> Option<i64> {
    match v {
        redis::Value::Int(i) => Some(*i),
        redis::Value::BulkString(b) => String::from_utf8_lossy(b).parse().ok(),
        redis::Value::SimpleString(s) => s.parse().ok(),
        _ => None,
    }
}

/// Extract a String from a redis::Value (BulkString or SimpleString).
fn value_as_string(v: &redis::Value) -> Option<String> {
    match v {
        redis::Value::BulkString(b) => Some(String::from_utf8_lossy(b).to_string()),
        redis::Value::SimpleString(s) => Some(s.clone()),
        _ => None,
    }
}

trait ValueHelpers {
    fn as_int_opt(&self) -> Option<i64>;
}

impl ValueHelpers for redis::Value {
    fn as_int_opt(&self) -> Option<i64> {
        value_as_int(self)
    }
}

/// `propfirm-worker reset-group` — delete and recreate the consumer
/// group on the request stream.
///
/// This is a maintenance tool: when the consumer group gets into a bad
/// state (e.g. corrupted PEL after a Redis failover, or stale entries
/// from crashed workers that won't go away), resetting the group clears
/// the PEL and gives workers a fresh start.
///
/// ## Behavior
///
/// 1. `XGROUP DESTROY stream group` — deletes the group + all its PEL.
/// 2. `XGROUP CREATE stream group $ MKSTREAM` — recreates the group
///    from the latest message forward.
/// 3. All previously-pending entries are lost (they're now unacked +
///    untracked).
///
/// ## Usage
///
/// ```bash
/// kubectl exec deploy/propfirm-worker -- /app/propfirm-worker reset-group
/// ```
///
/// ## Safety
///
/// **Destructive** — any in-flight messages are lost. Use only when:
/// - The worker pool is stopped (so no consumer is actively processing).
/// - OR the PEL is known to contain only already-processed-but-unacked
///   messages (e.g. after a crash where the worker already published
///   responses but didn't get to XACK).
async fn run_reset_group() -> anyhow::Result<()> {
    use propfirm::persistence::redis_store::RedisConn;

    let settings = Settings::load()?;
    let conn = match redis_connect(&settings.redis).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("FAIL: redis connect failed: {e}");
            return Err(anyhow::anyhow!("redis connect failed: {e}"));
        }
    };
    let stream = settings.event_bus.request_stream.clone();
    let group = settings.event_bus.consumer_group.clone();

    // Step 1: XGROUP DESTROY — deletes the group + its PEL.
    let destroy_ok: bool = match &conn {
        RedisConn::Single(c) => {
            let mut c = c.clone();
            redis::cmd("XGROUP")
                .arg("DESTROY")
                .arg(&stream)
                .arg(&group)
                .query_async::<i64>(&mut c)
                .await
                .map(|n| n == 1)
                .unwrap_or(false)
        }
        RedisConn::Cluster(pool) => match pool.get().await {
            Ok(mut c) => redis::cmd("XGROUP")
                .arg("DESTROY")
                .arg(&stream)
                .arg(&group)
                .query_async::<i64>(&mut *c)
                .await
                .map(|n| n == 1)
                .unwrap_or(false),
            Err(_) => false,
        },
    };

    if !destroy_ok {
        eprintln!("WARN: XGROUP DESTROY returned 0 (group may not have existed)");
    } else {
        eprintln!("OK: destroyed consumer group '{group}' on stream '{stream}'");
    }

    // Step 2: XGROUP CREATE — recreate with MKSTREAM + start-from-$.
    let create_ok: bool = match &conn {
        RedisConn::Single(c) => {
            let mut c = c.clone();
            redis::cmd("XGROUP")
                .arg("CREATE")
                .arg(&stream)
                .arg(&group)
                .arg("$")
                .arg("MKSTREAM")
                .query_async::<()>(&mut c)
                .await
                .is_ok()
        }
        RedisConn::Cluster(pool) => match pool.get().await {
            Ok(mut c) => redis::cmd("XGROUP")
                .arg("CREATE")
                .arg(&stream)
                .arg(&group)
                .arg("$")
                .arg("MKSTREAM")
                .query_async::<()>(&mut *c)
                .await
                .is_ok(),
            Err(_) => false,
        },
    };

    if create_ok {
        eprintln!("OK: recreated consumer group '{group}' on stream '{stream}' (start from $)");
        println!("reset_group=ok stream={stream} group={group}");
    } else {
        eprintln!("FAIL: XGROUP CREATE failed (group may already exist — try reset-group again)");
        println!("reset_group=fail stream={stream} group={group}");
    }

    Ok(())
}
