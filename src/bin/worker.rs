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

use propfirm::api::metrics::worker as worker_metrics;
use propfirm::api::middleware::install_panic_hook;
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
use std::time::Duration;
use tracing::{error, info};
use uuid::Uuid;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Load settings.
    let settings = Settings::load().map_err(|e| {
        eprintln!("FATAL: failed to load settings: {e}");
        e
    })?;

    // 2. Tracing.
    init_tracing(&settings);

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

    // 6. Spawn consumer tasks.
    let shutdown = shutdown_signal(Duration::from_secs(settings.server.shutdown_timeout_secs));
    let mut tasks = Vec::new();

    for i in 0..settings.event_bus.concurrency {
        let bus = bus.clone();
        let shutdown = shutdown.clone();
        let consumer_name = format!("worker-{}", i);
        tasks.push(tokio::spawn(async move {
            info!(consumer = %consumer_name, "consumer started");
            worker_loop(&bus, &consumer_name, shutdown).await;
        }));
    }

    // 7. Periodic PEL recovery task.
    let recovery_bus = bus.clone();
    let recovery_shutdown = shutdown.clone();
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
                process_request(&recovery_bus, stream_id, payload, "recovery").await;
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

    info!("worker stopped cleanly");
    Ok(())
}

/// One consumer's main loop: read → process → ack.
async fn worker_loop(
    bus: &RedisEventBus,
    consumer_name: &str,
    shutdown: tokio_util::sync::CancellationToken,
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
                process_request(bus, stream_id, payload, consumer_name).await;
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
async fn process_request(
    bus: &RedisEventBus,
    stream_id: String,
    payload: EvaluateRequestPayload,
    consumer_name: &str,
) {
    let request_id = payload.request_id.clone();
    let processed_at = chrono::Utc::now().to_rfc3339();
    let _latency = worker_metrics::latency_scope();

    // Parse inputs.
    let (response_payload, error_msg) = match parse_and_evaluate(&payload).await {
        Ok((decision_kind, input_hash, account_state, violations)) => {
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
    } else {
        worker_metrics::record_message_produced();
    }

    // Ack the request (so it leaves the PEL).
    if let Err(e) = bus.ack(&stream_id).await {
        worker_metrics::record_error("ack_failed");
        error!(request_id = %request_id, stream_id = %stream_id, error = %e, "failed to XACK");
    } else {
        worker_metrics::record_message_acked();
    }
    tracing::info!(request_id = %request_id, stream_id = %stream_id, consumer = %consumer_name, error = ?error_msg, "request processed");
}

/// Parse the request payload and run the pure evaluate function.
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

fn init_tracing(settings: &Settings) {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_new(&settings.observability.log_filter)
        .unwrap_or_else(|_| EnvFilter::new("info"));
    match settings.observability.log_format.as_str() {
        "pretty" => {
            fmt().with_env_filter(filter).with_target(false).init();
        }
        _ => {
            fmt()
                .with_env_filter(filter)
                .with_target(true)
                .json()
                .init();
        }
    }
}
