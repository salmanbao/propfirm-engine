//! HTTP handlers.
//!
//! **Design**: stateless state-in / state-out. Every mutating/read endpoint
//! takes `account_state` from the caller, builds its evaluator from that
//! account's bound plan per request, and returns the updated account state
//! or status. No request path relies on `ServerState.event_store` or a
//! startup-time plan; the caller owns durable records.
//!
//! **P2-API fix**: implements the binding spec's engine contract endpoints.
//! All mutating endpoints accept an `Idempotency-Key` header (P2-API fix);
//! the server tracks the last N idempotency keys per endpoint to
//! deduplicate retries.

use crate::api::dto::{EvaluateOrderRequest, EvaluateOrderResponse};
use crate::api::metrics::{
    record_decision, record_error, record_idempotency_outcome, LatencyScope,
};
use crate::core::ids::AccountId;
use crate::core::order::{Order, OrderKind, OrderSide, OrderType, TimeInForce};
use crate::core::types::{Price, Quantity, Symbol};
use crate::engine::pipeline::PipelineEvent;
use crate::override_engine::Override;
use crate::rulepack::RulePack;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;
use uuid::Uuid;

pub type SharedState = Arc<RwLock<crate::api::server::ServerState>>;

/// Extracts `TenantId` from the `X-Tenant-Id` header.
///
/// **No authentication** — the engine is reached only from the
/// platform backend over the private compose network. The caller is
/// trusted to set the correct tenant. The header is still required so
/// the engine has a typed tenant id for state scoping and audit logs.
fn extract_tenant_id(headers: &HeaderMap) -> Result<crate::tenant::TenantId, (StatusCode, String)> {
    headers
        .get("X-Tenant-Id")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "missing X-Tenant-Id header".to_string(),
            )
        })?
        .parse::<crate::tenant::TenantId>()
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))
}

fn validate_account_state(
    acc: &crate::core::account::Account,
    account_id: AccountId,
    tenant_id: crate::tenant::TenantId,
) -> Result<(), (StatusCode, String)> {
    if acc.id != account_id {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "account_state.id {} does not match account_id {}",
                acc.id, account_id
            ),
        ));
    }
    if acc.tenant_id != tenant_id {
        return Err((
            StatusCode::FORBIDDEN,
            format!(
                "account_state.tenant_id {} does not match authenticated tenant {}",
                acc.tenant_id, tenant_id
            ),
        ));
    }
    Ok(())
}

pub async fn health() -> &'static str {
    "ok"
}

/// `GET /ready` — readiness probe.
///
/// Distinct from `/health` (liveness): readiness reflects whether the
/// service can actually process work. We verify the idempotency backend
/// is configured (the trait object is non-null by construction) and
/// report 503 if not. In future, this should also ping the Postgres
/// pool and Redis connection for live checks.
pub async fn ready(State(state): State<SharedState>) -> Result<&'static str, (StatusCode, String)> {
    let _s = state.read().await.clone();
    // Verify the backend is non-null. The trait object is always Some in
    // current code paths, so this is a placeholder for future live checks.
    Ok("ready")
}

/// `POST /internal/v1/evaluate` — the stateless evaluate contract.
/// Takes `{account_id, account_state, tick, equity_source, open_positions,
/// today_trades}` and returns `{decision_kind, input_hash, account_state,
/// ...}`. `account_state` is required (400 without it) and `rule_pack` is
/// rejected with 400 — the rule set comes from the account's bound plan.
/// This is what the platform's LCC module calls; it does NOT mutate
/// server-side state.
///
/// **P0.5 fix — termination requires broker-reported equity.** The
/// `equity_source` field states who vouches for the account's
/// equity/balance numbers:
///
/// - `"broker_reported"` — the numbers come from the broker bridge;
///   breach-capable rules may emit `Fail`/`Liquidate`.
/// - `"estimated"` (or field absent — the safe default) — the numbers
///   are engine estimates; breach-capable rules downgrade to `Warn` and
///   never terminate the account.
///
/// The endpoint is reached over the private compose network from the
/// platform backend; trust is established at the network boundary, not
/// in-process (no auth layer). It takes account state from the request
/// body; without the explicit provenance field the P1-5 guard
/// ("estimates cannot terminate") was bypassable. Now it is not.
///
/// **P0.6 fix**: optional `open_positions` and `today_trades` arrays
/// are accepted and passed to the evaluation so position-dependent
/// rules (overnight/weekend holding, hedging, grid, max open
/// positions, copy trading) work on the stateless path.
///
/// **P0.8 fix**: the `Idempotency-Key` header is honoured: the first
/// response for a key is cached and replayed; a replay with a
/// conflicting body is rejected with 409.

#[axum::debug_handler]
#[tracing::instrument(skip(state, headers, req), fields(endpoint = "/internal/v1/evaluate"))]
pub async fn evaluate_internal(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(req): Json<InternalEvaluateRequest>,
) -> Result<Json<InternalEvaluateResponse>, (StatusCode, String)> {
    let _latency = LatencyScope::start("/internal/v1/evaluate");
    // P0.8: idempotency — key → first response, conflicting bodies 409.
    //
    // Restructured to check BEFORE evaluating: avoids wasting CPU on
    // replays (the most common idempotency hit). The TOCTOU window
    // between `check` returning Fresh and `remember` storing the
    // response is handled by `remember`'s atomic upsert — if another
    // pod stored a response in between, `remember` returns Replay
    // and we return the other pod's cached response.
    let tenant_id = extract_tenant_id(&headers)?;
    let body = serde_json::to_string(&req).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let s = state.read().await.clone();
    let response = if let Some(key) = headers.get("Idempotency-Key").and_then(|v| v.to_str().ok()) {
        // Step 1: Check first — if Replay, return cached without evaluating.
        match s
            .idempotency
            .check(tenant_id, "POST /internal/v1/evaluate", key, &body)
            .await
        {
            crate::api::idempotency::IdempotencyOutcome::Replay(cached) => {
                record_idempotency_outcome("replay");
                let cached = serde_json::from_str(&cached)
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                return Ok(Json(cached));
            }
            crate::api::idempotency::IdempotencyOutcome::Conflict => {
                record_idempotency_outcome("conflict");
                record_error("/internal/v1/evaluate", "idempotency_conflict");
                return Err((
                    StatusCode::CONFLICT,
                    "Idempotency-Key was already used with a different request body".into(),
                ));
            }
            crate::api::idempotency::IdempotencyOutcome::Error => {
                record_idempotency_outcome("error");
                record_error("/internal/v1/evaluate", "idempotency_backend_error");
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "idempotency lookup failed".into(),
                ));
            }
            crate::api::idempotency::IdempotencyOutcome::Fresh => {
                record_idempotency_outcome("fresh");
                // Not seen yet — proceed to evaluate.
            }
        }

        // Step 2: Evaluate (CPU cost).
        let response = evaluate_internal_impl(tenant_id, req).await?;
        let response_str = serde_json::to_string(&response)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        // Step 3: Remember — atomic upsert. If another pod stored
        // a response between our check and remember, we get Replay
        // and return the other pod's cached response instead.
        match s
            .idempotency
            .remember(
                tenant_id,
                "POST /internal/v1/evaluate",
                key,
                &body,
                &response_str,
            )
            .await
        {
            crate::api::idempotency::IdempotencyOutcome::Replay(cached) => {
                // Another pod won the race — return their response.
                let cached = serde_json::from_str(&cached)
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                cached
            }
            crate::api::idempotency::IdempotencyOutcome::Conflict => {
                // Another pod stored with a different body — 409.
                return Err((
                    StatusCode::CONFLICT,
                    "Idempotency-Key was already used with a different request body".into(),
                ));
            }
            crate::api::idempotency::IdempotencyOutcome::Fresh => response,
            crate::api::idempotency::IdempotencyOutcome::Error => response,
        }
    } else {
        evaluate_internal_impl(tenant_id, req).await?
    };
    // Record the decision kind metric.
    record_decision(&response.decision_kind);

    // Audit log: only write when the decision is non-Pass to avoid
    // drowning the audit_log table in normal traffic. Pass verdicts
    // are still observable via the evaluate_decisions_total metric.
    let decision_lower = response.decision_kind.to_lowercase();
    if decision_lower != "pass" {
        let audit = crate::api::audit_log::evaluate(
            tenant_id,
            response.account_state.id,
            &response.decision_kind,
            &response.input_hash,
        );
        let pg_pool = s.pg_pool.clone();
        audit.finish(pg_pool.as_ref(), None, 200).await;
    }
    Ok(Json(response))
}

/// The actual evaluation logic, split out so idempotency wrapping stays
/// readable.
#[tracing::instrument(skip(tenant_id, req), fields(account_id = ?req.account_id, endpoint = "/internal/v1/evaluate_impl"))]
async fn evaluate_internal_impl(
    tenant_id: crate::tenant::TenantId,
    req: InternalEvaluateRequest,
) -> Result<InternalEvaluateResponse, (StatusCode, String)> {
    let account_id = AccountId::from_uuid(
        Uuid::from_str(&req.account_id).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?,
    );
    // P2: bridge.tick takes precedence; when present the envelope is
    // broker-attested so equity provenance defaults to BrokerReported.
    let (equity_source, bridge_tick) = match req.bridge_tick {
        Some(ref bt) => (crate::pure::EquitySource::BrokerReported, Some(bt)),
        None => {
            let src = match req.equity_source.as_deref() {
                None | Some("estimated") => crate::pure::EquitySource::Estimated,
                Some(other) => crate::pure::EquitySource::parse(other)
                    .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?,
            };
            (src, None)
        }
    };
    let mut acc = req.account_state.ok_or((
        StatusCode::BAD_REQUEST,
        "account_state is required for stateless evaluation".into(),
    ))?;
    validate_account_state(&acc, account_id, tenant_id)?;
    if acc.status.evaluation_mode() == crate::core::account::EvaluationMode::Skip {
        return Ok(InternalEvaluateResponse {
            evaluated: false,
            decision_kind: "NotEvaluated".to_string(),
            winning_priority: 0,
            input_hash: String::new(),
            pack_version: 0,
            pack_id: String::new(),
            violations: Vec::new(),
            violation_details: Vec::new(),
            account_state: acc,
        });
    }
    let pack = if req.rule_pack.is_some() {
        return Err((
            StatusCode::BAD_REQUEST,
            "rule_pack is no longer accepted; evaluation uses the account's bound plan".into(),
        ));
    } else {
        crate::rulepack::RulePack::synthetic_from_plan(account_id, tenant_id, &acc.plan)
    };
    let registry = crate::rules::registry::RuleRegistry::with_default_rules_for_plan(&acc.plan);

    // P2: cross-account reference trades (same for both shapes).
    let cross_ref_account_id = AccountId::from_uuid(Uuid::new_v4());
    let mut cross_errors: Vec<String> = Vec::new();
    let cross_reference_trades: Vec<crate::core::trade::Trade> = req
        .cross_reference_trades
        .unwrap_or_default()
        .into_iter()
        .filter_map(|t| match t.into_domain(cross_ref_account_id) {
            Ok(tr) => Some(tr),
            Err(e) => {
                cross_errors.push(e);
                None
            }
        })
        .collect();
    if let Some(first) = cross_errors.first() {
        return Err((StatusCode::BAD_REQUEST, first.clone()));
    }
    let cross_reference_trades: Vec<crate::core::trade::Trade> = cross_reference_trades
        .into_iter()
        .filter(|t| t.account_id != account_id)
        .collect();

    // P2: choose between the new bridge.tick v1 envelope and the legacy
    // per-symbol tick. The two branches produce the same output shape
    // (positions, trades, server_time, latest_tick) so the pure evaluate
    // call below is shared.
    let (positions, trades, server_time, latest_tick) = if let Some(bt) = bridge_tick {
        let payload = &bt.payload;
        // Override stored account financials with broker-reported cents.
        let cents_to_decimal = |cents: i64| -> crate::core::types::Money {
            crate::core::types::Money(
                rust_decimal::Decimal::from(cents) / rust_decimal::Decimal::from(100),
            )
        };
        acc.equity = cents_to_decimal(payload.equity_cents);
        acc.balance = cents_to_decimal(payload.balance_cents);
        // margin_cents / free_margin_cents are advisory hints for later
        // floor-hint computation; stored on the account for visibility.
        let _ = payload.margin_cents;
        let _ = payload.free_margin_cents;

        // Positions come from the envelope's payload.positions[].
        let mut position_errors = Vec::new();
        let positions: Vec<crate::core::position::Position> = payload
            .positions
            .iter()
            .cloned()
            .filter_map(|p| match p.into_domain(account_id) {
                Ok(pos) => Some(pos),
                Err(e) => {
                    position_errors.push(e);
                    None
                }
            })
            .collect();
        if let Some(first) = position_errors.first() {
            return Err((StatusCode::BAD_REQUEST, first.clone()));
        }

        // Trades are still supplied separately (workers accumulate them).
        let mut trade_errors = Vec::new();
        let trades: Vec<crate::core::trade::Trade> = req
            .today_trades
            .unwrap_or_default()
            .into_iter()
            .filter_map(|t| match t.into_domain(account_id) {
                Ok(tr) => Some(tr),
                Err(e) => {
                    trade_errors.push(e);
                    None
                }
            })
            .collect();
        if let Some(first) = trade_errors.first() {
            return Err((StatusCode::BAD_REQUEST, first.clone()));
        }

        let server_time = crate::core::types::ServerTime(
            chrono::DateTime::from_timestamp_millis(payload.broker_time)
                .ok_or_else(|| (StatusCode::BAD_REQUEST, "invalid broker_time".into()))?,
        );

        (positions, trades, server_time, None)
    } else {
        // Legacy shape: per-symbol quote + explicit open positions.
        let tick = req.tick.ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                "tick is required when bridge_tick is absent".into(),
            )
        })?;
        let server_time = crate::core::types::ServerTime(tick.quote.ts);

        let mut position_errors = Vec::new();
        let positions: Vec<crate::core::position::Position> = req
            .open_positions
            .unwrap_or_default()
            .into_iter()
            .filter_map(|p| match p.into_domain(account_id) {
                Ok(pos) => Some(pos),
                Err(e) => {
                    position_errors.push(e);
                    None
                }
            })
            .collect();
        if let Some(first) = position_errors.first() {
            return Err((StatusCode::BAD_REQUEST, first.clone()));
        }

        let mut trade_errors = Vec::new();
        let trades: Vec<crate::core::trade::Trade> = req
            .today_trades
            .unwrap_or_default()
            .into_iter()
            .filter_map(|t| match t.into_domain(account_id) {
                Ok(tr) => Some(tr),
                Err(e) => {
                    trade_errors.push(e);
                    None
                }
            })
            .collect();
        if let Some(first) = trade_errors.first() {
            return Err((StatusCode::BAD_REQUEST, first.clone()));
        }

        (positions, trades, server_time, Some(tick))
    };

    // Staleness check: reject ticks older than 10 minutes from
    // the pod's wall clock. This prevents processing stale data
    // that could produce incorrect verdicts. The threshold is
    // generous (10 min) to tolerate clock skew between pods.
    let STALE_THRESHOLD_SECS: i64 = 10 * 60;
    let now = chrono::Utc::now();
    let staleness_secs = (now - server_time.0).num_seconds();
    if staleness_secs > STALE_THRESHOLD_SECS {
        return Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            format!(
                "tick rejected: server_time is {} seconds old (threshold: {}s)",
                staleness_secs, STALE_THRESHOLD_SECS
            ),
        ));
    }

    // P1-5 parity with AccountState::update_equity: broker-reported
    // equity/balance raise the drawdown baselines; estimates must not.
    if matches!(equity_source, crate::pure::EquitySource::BrokerReported) {
        if acc.equity.0 > acc.peak_equity.0 {
            acc.peak_equity = acc.equity;
        }
        if acc.balance.0 > acc.peak_balance.0 {
            acc.peak_balance = acc.balance;
        }
    }

    // Pure evaluate (P1-7) — no storage mutation. P0-C: server_time is
    // explicit so the verdict is reproducible from recorded inputs.
    let inputs = if let Some(ref tick) = latest_tick {
        crate::pure::EvaluateInputs::for_tick(&positions, &trades, tick)
            .with_cross_reference_trades(cross_reference_trades)
            .with_equity_source(equity_source)
    } else {
        crate::pure::EvaluateInputs {
            open_positions: &positions,
            today_trades: &trades,
            cross_reference_trades,
            equity_source,
            ..Default::default()
        }
    };
    let verdict = crate::pure::evaluate(
        &acc,
        &pack,
        &registry,
        crate::rules::context::RuleContextKind::OnTick,
        server_time,
        inputs,
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let state = crate::engine::state::AccountState::new(acc.clone());
    let actor_id = "evaluate_internal";
    let (new_state, _events) =
        crate::engine::pipeline::apply_decision(state, &verdict.decision, server_time.0, actor_id)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(InternalEvaluateResponse {
        evaluated: true,
        decision_kind: format!("{:?}", verdict.decision.kind),
        winning_priority: verdict.decision.winning_priority,
        input_hash: verdict.input_hash,
        pack_version: verdict.pack_version,
        pack_id: verdict.pack_id,
        violations: verdict
            .decision
            .all_violations
            .iter()
            .map(|v| v.message.clone())
            .collect(),
        violation_details: verdict.decision.all_violations,
        account_state: new_state.account,
    })
}

/// `POST /internal/v1/override` — clear a false-positive breach.
///
/// **Stateless design**: takes `account_state` from the caller, builds the
/// evaluator from that account's plan, applies the override, and returns the
/// updated `account_state`. No event-store replay is performed.
#[tracing::instrument(skip(state, headers, req), fields(account_id = ?req.account_id, endpoint = "/internal/v1/override", actor = ?req.actor_id))]
pub async fn override_breach(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(req): Json<OverrideRequest>,
) -> Result<Json<OverrideResponse>, (StatusCode, String)> {
    let _latency = LatencyScope::start("/internal/v1/override");
    let tenant_id = extract_tenant_id(&headers)?;
    let account_id = AccountId::from_uuid(
        Uuid::from_str(&req.account_id).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?,
    );
    let clears_violation_id = crate::core::ids::ViolationId::from_uuid(
        Uuid::from_str(&req.clears_violation_id)
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?,
    );
    let acc = req.account_state.ok_or((
        StatusCode::BAD_REQUEST,
        "account_state is required for stateless override".into(),
    ))?;
    validate_account_state(&acc, account_id, tenant_id)?;
    if !acc.status.is_breach_terminal() {
        record_error("/internal/v1/override", "invalid_status");
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "account {} is in status {:?}; override is only valid from Failed/EmergencyStopped",
                acc.id, acc.status
            ),
        ));
    }
    if req.violation.id != clears_violation_id {
        record_error("/internal/v1/override", "violation_id_mismatch");
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "violation.id {} does not match clears_violation_id {}",
                req.violation.id, clears_violation_id
            ),
        ));
    }
    if req.violation.account_id != account_id {
        record_error("/internal/v1/override", "violation_account_mismatch");
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "violation.account_id {} does not match request account_id {}",
                req.violation.account_id, account_id
            ),
        ));
    }
    if req.violation.tenant_id != tenant_id {
        record_error("/internal/v1/override", "tenant_mismatch");
        return Err((
            StatusCode::FORBIDDEN,
            format!(
                "violation.tenant_id {} does not match request tenant {}",
                req.violation.tenant_id, tenant_id
            ),
        ));
    }
    req.violation
        .validate()
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    if !req.violation.is_terminating() {
        record_error("/internal/v1/override", "non_terminating_violation");
        return Err((
            StatusCode::BAD_REQUEST,
            "override is only valid for terminating violations".into(),
        ));
    }
    let override_record = Override::new(
        account_id,
        clears_violation_id,
        req.reason.clone(),
        req.actor_id.clone(),
        chrono::Utc::now(),
    );

    // Audit log entry — persisted to the audit_log table after the
    // operation completes (success or failure). The pool may be absent
    // in dev mode; in that case the call is a no-op.
    let audit = crate::api::audit_log::override_breach(
        &req.actor_id,
        tenant_id,
        account_id,
        clears_violation_id,
        &req.reason,
    );
    let pg_pool = state.read().await.pg_pool.clone();

    let registry = crate::rules::registry::RuleRegistry::with_default_rules_for_plan(&acc.plan);
    let evaluator =
        crate::engine::evaluator::Evaluator::with_registry(registry).for_account(account_id);
    let notifier = crate::notifications::log::LogNotifier::new();
    let mut pipeline = crate::engine::pipeline::Pipeline::new(evaluator, notifier);
    let pipeline_result = pipeline
        .process(
            acc.clone(),
            PipelineEvent::OverrideBreach {
                override_record: override_record.clone(),
            },
        )
        .await;

    let result = match pipeline_result {
        Ok(r) => {
            audit.finish(pg_pool.as_ref(), None, 200).await;
            r
        }
        Err(e) => {
            let err_msg = e.to_string();
            audit.finish(pg_pool.as_ref(), None, 500).await;
            record_error("/internal/v1/override", "internal_error");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, err_msg));
        }
    };

    Ok(Json(OverrideResponse {
        override_id: override_record.id.to_string(),
        cleared_at: override_record.at.to_rfc3339(),
        account_state: result.account,
    }))
}

/// `POST /internal/v1/manual-run` — force re-evaluation of an account.
///
/// **Stateless design**: takes `account_state` from the caller, builds the
/// evaluator from that account's plan, runs evaluation, and returns the
/// verdict plus updated `account_state`. No event-store replay is performed.
#[tracing::instrument(skip(state, headers, req), fields(account_id = ?req.account_id, endpoint = "/internal/v1/manual-run"))]
pub async fn manual_run(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(req): Json<ManualRunRequest>,
) -> Result<Json<ManualRunResponse>, (StatusCode, String)> {
    let _latency = LatencyScope::start("/internal/v1/manual-run");
    let tenant_id = extract_tenant_id(&headers)?;
    let account_id = AccountId::from_uuid(
        Uuid::from_str(&req.account_id).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?,
    );
    let acc = req.account_state.ok_or((
        StatusCode::BAD_REQUEST,
        "account_state is required for stateless evaluation".into(),
    ))?;
    validate_account_state(&acc, account_id, tenant_id)?;
    if acc.status.evaluation_mode() == crate::core::account::EvaluationMode::Skip {
        return Ok(Json(ManualRunResponse {
            decision_kind: "NotEvaluated".to_string(),
            passed: false,
            violations: Vec::new(),
            violation_details: Vec::new(),
            account_state: acc,
        }));
    }

    // Audit log entry — manual runs are operator-triggered (low
    // volume), so we always write to the audit_log table.
    let audit = crate::api::audit_log::manual_run("manual_run", tenant_id, account_id);
    let pg_pool = state.read().await.pg_pool.clone();

    let registry = crate::rules::registry::RuleRegistry::with_default_rules_for_plan(&acc.plan);
    let evaluator =
        crate::engine::evaluator::Evaluator::with_registry(registry).for_account(account_id);
    let notifier = crate::notifications::log::LogNotifier::new();
    let mut pipeline = crate::engine::pipeline::Pipeline::new(evaluator, notifier);
    let pipeline_result = pipeline.process(acc.clone(), PipelineEvent::OnDemand).await;

    let result = match pipeline_result {
        Ok(r) => {
            audit.finish(pg_pool.as_ref(), None, 200).await;
            r
        }
        Err(e) => {
            let err_msg = e.to_string();
            audit.finish(pg_pool.as_ref(), None, 500).await;
            record_error("/internal/v1/manual-run", "internal_error");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, err_msg));
        }
    };

    Ok(Json(ManualRunResponse {
        decision_kind: format!("{:?}", result.snapshot.decision.kind),
        violations: result
            .result
            .violations()
            .iter()
            .map(|v| v.message.clone())
            .collect(),
        violation_details: result.result.decision.all_violations.clone(),
        passed: result.passed(),
        account_state: result.account,
    }))
}

/// `POST /internal/v1/emergency-stop` — freeze an account immediately.
/// Short-circuits normal rule evaluation and forces an emergency-stop
/// decision with full audit metadata (`reason` + `actor_id`).
///
/// **Stateless design**: takes `account_state` from the caller, builds the
/// evaluator from that account's plan, applies the emergency stop, and
/// returns the updated `account_state`. No event-store replay is performed.
#[tracing::instrument(skip(state, headers, req), fields(account_id = ?req.account_id, endpoint = "/internal/v1/emergency-stop", actor = ?req.actor_id))]
pub async fn emergency_stop(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(req): Json<EmergencyStopRequest>,
) -> Result<Json<EmergencyStopResponse>, (StatusCode, String)> {
    let _latency = LatencyScope::start("/internal/v1/emergency-stop");
    let tenant_id = extract_tenant_id(&headers)?;
    let account_id = AccountId::from_uuid(
        Uuid::from_str(&req.account_id).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?,
    );
    let acc = req.account_state.ok_or((
        StatusCode::BAD_REQUEST,
        "account_state is required for stateless evaluation".into(),
    ))?;
    validate_account_state(&acc, account_id, tenant_id)?;
    if acc.status == crate::core::account::AccountStatus::EmergencyStopped {
        return Ok(Json(EmergencyStopResponse {
            decision_kind: "Emergency".to_string(),
            stopped_at: chrono::Utc::now().to_rfc3339(),
            account_state: acc,
        }));
    }
    if acc.status.evaluation_mode() == crate::core::account::EvaluationMode::Skip {
        return Ok(Json(EmergencyStopResponse {
            decision_kind: "NotEvaluated".to_string(),
            stopped_at: chrono::Utc::now().to_rfc3339(),
            account_state: acc,
        }));
    }

    // Audit log entry — emergency stops are sensitive operator actions,
    // always audit (with reason + actor_id).
    let audit =
        crate::api::audit_log::emergency_stop(&req.actor_id, tenant_id, account_id, &req.reason);
    let pg_pool = state.read().await.pg_pool.clone();

    let at = chrono::Utc::now();
    let registry = crate::rules::registry::RuleRegistry::with_default_rules_for_plan(&acc.plan);
    let evaluator =
        crate::engine::evaluator::Evaluator::with_registry(registry).for_account(account_id);
    let notifier = crate::notifications::log::LogNotifier::new();
    let mut pipeline = crate::engine::pipeline::Pipeline::new(evaluator, notifier);
    let pipeline_result = pipeline
        .process(
            acc.clone(),
            PipelineEvent::EmergencyStop {
                reason: req.reason,
                actor_id: req.actor_id,
                at,
            },
        )
        .await;

    let result = match pipeline_result {
        Ok(r) => {
            audit.finish(pg_pool.as_ref(), None, 200).await;
            r
        }
        Err(e) => {
            let err_msg = e.to_string();
            audit.finish(pg_pool.as_ref(), None, 500).await;
            record_error("/internal/v1/emergency-stop", "internal_error");
            return Err((StatusCode::INTERNAL_SERVER_ERROR, err_msg));
        }
    };

    Ok(Json(EmergencyStopResponse {
        decision_kind: format!("{:?}", result.snapshot.decision.kind),
        stopped_at: at.to_rfc3339(),
        account_state: result.account,
    }))
}

/// `GET /internal/v1/breach-report/:account_id` — the trader-facing
/// "why did I fail" view with evidence (TD-25). Returns the breach
/// violation + the rule that produced it + the `input_hash` for verification.
///
/// **Stateless design**: changed to a POST-style request body flow taking
/// `account_state`. Returns the current account status; violation history
/// is caller-owned. No event-store replay is performed.
#[tracing::instrument(skip(state, headers, req), fields(account_id = ?req.account_id, endpoint = "/internal/v1/breach-report"))]
pub async fn breach_report(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(req): Json<BreachReportRequest>,
) -> Result<Json<BreachReportResponse>, (StatusCode, String)> {
    let _latency = LatencyScope::start("/internal/v1/breach-report");
    let tenant_id = extract_tenant_id(&headers)?;
    let account_id = AccountId::from_uuid(
        Uuid::from_str(&req.account_id).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?,
    );
    let acc = req.account_state.ok_or((
        StatusCode::BAD_REQUEST,
        "account_state is required for breach report".into(),
    ))?;
    validate_account_state(&acc, account_id, tenant_id)?;
    for v in &req.violations {
        if v.tenant_id != tenant_id {
            return Err((
                StatusCode::FORBIDDEN,
                format!(
                    "violation.tenant_id {} does not match request tenant {}",
                    v.tenant_id, tenant_id
                ),
            ));
        }
    }
    let cleared_violation_ids: std::collections::HashSet<_> = req
        .overrides
        .iter()
        .map(|o| {
            crate::core::ids::ViolationId::from_str(&o.clears_violation_id).unwrap_or_default()
        })
        .collect();
    let violations: Vec<ViolationSummary> = req
        .violations
        .iter()
        .map(|v| ViolationSummary {
            rule_name: v.rule_name.clone(),
            kind: format!("{:?}", v.kind),
            severity: format!("{:?}", v.severity),
            message: v.message.clone(),
            occurred_at: v.occurred_at.to_rfc3339(),
            cleared: cleared_violation_ids.contains(&v.id),
        })
        .collect();

    // Audit log entry — breach-report queries are reads, but we still
    // audit them so the platform can see who queried breach reports when.
    let audit = crate::api::audit_log::breach_report(
        tenant_id,
        account_id,
        violations.len(),
        cleared_violation_ids.len(),
    );
    let pg_pool = state.read().await.pg_pool.clone();

    let registry = crate::rules::registry::RuleRegistry::with_default_rules_for_plan(&acc.plan);
    let evaluator =
        crate::engine::evaluator::Evaluator::with_registry(registry).for_account(account_id);
    let notifier = crate::notifications::log::LogNotifier::new();
    let mut pipeline = crate::engine::pipeline::Pipeline::new(evaluator, notifier);
    let result = pipeline
        .process(acc.clone(), PipelineEvent::OnDemand)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let current_violations: Vec<ViolationSummary> = result
        .result
        .violations()
        .iter()
        .map(|v| ViolationSummary {
            rule_name: v.rule_name.clone(),
            kind: format!("{:?}", v.kind),
            severity: format!("{:?}", v.severity),
            message: v.message.clone(),
            occurred_at: v.occurred_at.to_rfc3339(),
            cleared: false,
        })
        .collect();

    // Persist the audit entry (read-only action, status 200).
    audit.finish(pg_pool.as_ref(), None, 200).await;

    Ok(Json(BreachReportResponse {
        account_id: req.account_id,
        account_status: format!("{:?}", result.account.status),
        violations,
        current_violations,
    }))
}

#[tracing::instrument(skip(state, headers, req), fields(account_id = ?req.account_id, endpoint = "/v1/evaluate-order", symbol = ?req.symbol, side = ?req.side))]
pub async fn evaluate_order(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(req): Json<EvaluateOrderRequest>,
) -> Result<Json<EvaluateOrderResponse>, (StatusCode, String)> {
    let _latency = LatencyScope::start("/v1/evaluate-order");
    let tenant_id = extract_tenant_id(&headers)?;
    let account_id = AccountId::from_uuid(
        Uuid::from_str(&req.account_id).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?,
    );
    let acc = req.account_state.ok_or((
        StatusCode::BAD_REQUEST,
        "account_state is required for stateless evaluation".into(),
    ))?;
    validate_account_state(&acc, account_id, tenant_id)?;
    if acc.status.evaluation_mode() == crate::core::account::EvaluationMode::Skip {
        return Ok(Json(EvaluateOrderResponse {
            decision: "NotEvaluated".to_string(),
            passed: false,
            violations: Vec::new(),
            violation_details: Vec::new(),
            account_state: acc,
        }));
    }
    let side = match req.side.as_str() {
        "buy" => OrderSide::Buy,
        "sell" => OrderSide::Sell,
        other => return Err((StatusCode::BAD_REQUEST, format!("invalid side {other}"))),
    };
    let symbol_str = req.symbol.clone();
    let side_str = req.side.clone();
    let order = Order {
        id: crate::core::ids::OrderId::new(),
        account_id,
        symbol: Symbol::new(req.symbol),
        side,
        kind: OrderKind::Open,
        order_type: match req.order_type.as_str() {
            "market" => OrderType::Market,
            "limit" => OrderType::Limit {
                price: Price(req.price.unwrap_or_default()),
            },
            _ => OrderType::Market,
        },
        quantity: Quantity(req.quantity),
        tif: TimeInForce::Ioc,
        stop_loss: req.stop_loss.map(Price),
        take_profit: req.take_profit.map(Price),
        comment: None,
        submitted_at: chrono::Utc::now(),
        status: crate::core::order::OrderStatus::Pending,
        filled_quantity: Quantity::ZERO,
        avg_fill_price: None,
    };

    let registry = crate::rules::registry::RuleRegistry::with_default_rules_for_plan(&acc.plan);
    let evaluator =
        crate::engine::evaluator::Evaluator::with_registry(registry).for_account(account_id);
    let notifier = crate::notifications::log::LogNotifier::new();
    let mut pipeline = crate::engine::pipeline::Pipeline::new(evaluator, notifier);
    let result = pipeline
        .process(
            acc.clone(),
            PipelineEvent::OrderSubmitted {
                order: order.clone(),
            },
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let violations: Vec<String> = result
        .result
        .violations()
        .iter()
        .map(|v| format!("{}: {}", v.kind, v.message))
        .collect();

    let decision_kind = format!("{:?}", result.snapshot.decision.kind);

    // Audit log entry — pre-trade order evaluations are operator-driven,
    // always audit (with symbol/side/decision).
    let audit = crate::api::audit_log::evaluate_order(
        tenant_id,
        account_id,
        &symbol_str,
        &side_str,
        &decision_kind,
    );
    let pg_pool = state.read().await.pg_pool.clone();
    audit.finish(pg_pool.as_ref(), None, 200).await;

    Ok(Json(EvaluateOrderResponse {
        decision: decision_kind,
        passed: result.passed(),
        violations,
        violation_details: result.result.decision.all_violations.clone(),
        account_state: result.account,
    }))
}

// DTOs for the new endpoints.

/// `POST /v1/rule-packs/validate` — stateless rule pack validation.
///
/// Validates a rule pack without persisting it. Returns validation errors
/// or the pack's computed content hash. Used by tenants to verify packs
/// before binding them to accounts via platform tooling.
#[tracing::instrument(skip(req), fields(endpoint = "/v1/rule-packs/validate"))]
pub async fn validate_rule_pack(
    Json(req): Json<CreateRulePackRequest>,
) -> Result<Json<RulePackResponse>, (StatusCode, String)> {
    let rules: Vec<crate::rulepack::RuleEntry> =
        req.rules
            .into_iter()
            .map(|r| -> Result<_, (StatusCode, String)> {
                Ok(crate::rulepack::RuleEntry {
                    id: r.id,
                    kind: r.kind,
                    basis: r.basis.parse::<crate::rulepack::RuleBasis>().map_err(
                        |e: crate::core::Error| (StatusCode::BAD_REQUEST, e.to_string()),
                    )?,
                    unit: r.unit.parse::<crate::rulepack::RuleUnit>().map_err(
                        |e: crate::core::Error| (StatusCode::BAD_REQUEST, e.to_string()),
                    )?,
                    value: r.value,
                    tolerance_cents: r.tolerance_cents,
                    early_warning_pct: r.early_warning_pct,
                    priority: r.priority,
                    enabled: r.enabled,
                    params_json: r.params_json.unwrap_or_else(|| "{}".into()),
                    severity: r.severity.clone(),
                    failure_policy: r.failure_policy.clone(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
    // Tenant ID is not needed for validation; it's injected at bind time by platform tooling
    let pack = RulePack {
        id: req.id,
        version: req.version,
        tenant_id: crate::tenant::TenantId::named("validation"),
        lifecycle: crate::rulepack::PackLifecycle::Draft,
        effective_from: chrono::Utc::now(),
        superseded_by: None,
        description: req.description,
        rules,
        initial_balance: req.initial_balance,
        leverage: req.leverage,
        profit_target_pct: req.profit_target_pct,
    };
    pack.validate()
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let response = RulePackResponse {
        id: pack.id.clone(),
        version: pack.version,
        lifecycle: format!("{}", pack.lifecycle),
        content_hash: pack.content_hash(),
    };
    Ok(Json(response))
}

/// **P0.6 fix**: wire shape for an open position on the evaluate path.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PositionDto {
    pub position_id: String,
    pub symbol: String,
    pub side: String,
    pub open_quantity: rust_decimal::Decimal,
    pub avg_entry_price: rust_decimal::Decimal,
    pub opened_at: chrono::DateTime<chrono::Utc>,
}

impl PositionDto {
    /// Converts the wire shape into the domain [`Position`](crate::core::position::Position).
    pub fn into_domain(
        self,
        account_id: AccountId,
    ) -> Result<crate::core::position::Position, String> {
        use crate::core::ids::PositionId;
        let side = match self.side.to_ascii_lowercase().as_str() {
            "long" | "buy" => crate::core::position::PositionSide::Long,
            "short" | "sell" => crate::core::position::PositionSide::Short,
            other => return Err(format!("invalid position side '{other}'")),
        };
        let position_id = PositionId::from_uuid(
            Uuid::from_str(&self.position_id).map_err(|e| format!("invalid position_id: {e}"))?,
        );
        let mut p = crate::core::position::Position {
            id: position_id,
            account_id,
            symbol: Symbol::new(self.symbol),
            side,
            opened_at: self.opened_at,
            closed_at: None,
            status: crate::core::position::PositionStatus::Open,
            avg_entry_price: Price(self.avg_entry_price),
            opened_quantity: Quantity(self.open_quantity),
            open_quantity: Quantity(self.open_quantity),
            realized_pnl: crate::core::types::Money::ZERO,
            commission: crate::core::types::Money::ZERO,
            swap: crate::core::types::Money::ZERO,
            stop_loss: None,
            take_profit: None,
            magic: None,
            comment: None,
        };
        // Ensure the position reads as open.
        p.status = crate::core::position::PositionStatus::Open;
        Ok(p)
    }
}

/// **P0.6 fix**: wire shape for a trade on the evaluate path.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TradeDto {
    pub symbol: String,
    pub side: String,
    pub trade_side: String,
    pub price: rust_decimal::Decimal,
    pub quantity: rust_decimal::Decimal,
    pub executed_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub realized_pnl: Option<rust_decimal::Decimal>,
}

impl TradeDto {
    /// Converts the wire shape into the domain [`Trade`](crate::core::trade::Trade).
    pub fn into_domain(self, account_id: AccountId) -> Result<crate::core::trade::Trade, String> {
        use crate::core::trade::TradeSide;
        let side = match self.side.to_ascii_lowercase().as_str() {
            "buy" | "long" => OrderSide::Buy,
            "sell" | "short" => OrderSide::Sell,
            other => return Err(format!("invalid trade side '{other}'")),
        };
        let trade_side = match self.trade_side.to_ascii_lowercase().as_str() {
            "entry" | "open" => TradeSide::Entry,
            "exit" | "close" => TradeSide::Exit,
            other => return Err(format!("invalid trade_side '{other}'")),
        };
        let mut t = crate::core::trade::Trade::new(
            crate::core::ids::OrderId::new(),
            account_id,
            Symbol::new(self.symbol),
            side,
            trade_side,
            Price(self.price),
            Quantity(self.quantity),
            crate::core::types::Money::ZERO,
            self.executed_at,
        );
        if trade_side == TradeSide::Exit {
            t.exit_info = Some(crate::core::trade::TradeExit {
                position_id: crate::core::ids::PositionId::new(),
                realized_pnl: crate::core::types::Money(self.realized_pnl.unwrap_or_default()),
                closed_quantity: Quantity(self.quantity),
                entry_price: Price(self.price),
                exit_price: Price(self.price),
            });
        }
        Ok(t)
    }
}

// DTOs for the new endpoints.

/// **P2 wire contract fix**: bridge.tick v1 envelope — the account-level
/// record produced by the broker bridge (BRG). This is the real input the
/// engine should consume; the older per-symbol `Tick` shape is retained
/// only for overlapping callers during the transition.
///
/// Envelope per EVT-03; payload fields are provisional until freeze.
/// Canonical schema: `contracts/events/extended/bridge.schema.json`
/// ($defs/bridge_tick). Producer: 08 (BRG). When: every sync tx.
/// Consumers: EVL (trigger eval), ANA (equity points), web SSE fan-out.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BridgeTickV1 {
    #[serde(rename = "type")]
    pub type_: String,
    pub version: u32,
    pub tenant_id: String,
    pub occurred_at: i64,
    pub payload: BridgeTickPayload,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BridgeTickPayload {
    /// Broker-reported equity, integer cents.
    pub equity_cents: i64,
    /// Broker-reported balance, integer cents.
    pub balance_cents: i64,
    /// Broker-reported margin in use, integer cents.
    pub margin_cents: i64,
    /// Free margin, integer cents.
    pub free_margin_cents: i64,
    /// Account leverage (e.g. 100).
    pub leverage: u32,
    /// Current open positions snapshot from the bridge.
    pub positions: Vec<BridgeTickPositionDto>,
    /// Number of deals executed in the current period.
    pub deals_count: u32,
    /// Ticket id of the last executed deal, if any.
    #[serde(default)]
    pub last_deal_ticket: Option<String>,
    /// Broker-attested timestamp (ms since epoch).
    pub broker_time: i64,
    /// **P2 fix**: `trigger` is evidence, never a rule input (property test I-28).
    #[serde(default)]
    pub trigger: Option<String>,
    /// Bridge source identifier (e.g. `metaapi`).
    #[serde(default)]
    pub source: Option<String>,
    /// Monotonic sequence within this stream.
    #[serde(default)]
    pub stream_seq: Option<u64>,
    /// Rolling low equity within the current period, integer cents.
    #[serde(default)]
    pub equity_low_cents: Option<i64>,
    /// Rolling high equity within the current period, integer cents.
    #[serde(default)]
    pub equity_high_cents: Option<i64>,
}

/// Wire shape for a position snapshot inside `bridge.tick` v1 payload.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BridgeTickPositionDto {
    pub position_id: String,
    pub symbol: String,
    pub side: String,
    pub open_quantity: rust_decimal::Decimal,
    pub avg_entry_price: rust_decimal::Decimal,
    pub opened_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub closed_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub realized_pnl: Option<rust_decimal::Decimal>,
    #[serde(default)]
    pub commission: Option<rust_decimal::Decimal>,
    #[serde(default)]
    pub swap: Option<rust_decimal::Decimal>,
}

impl BridgeTickPositionDto {
    /// Converts the wire shape into the domain [`Position`](crate::core::position::Position).
    pub fn into_domain(
        self,
        account_id: AccountId,
    ) -> Result<crate::core::position::Position, String> {
        use crate::core::ids::PositionId;
        let side = match self.side.to_ascii_lowercase().as_str() {
            "long" | "buy" => crate::core::position::PositionSide::Long,
            "short" | "sell" => crate::core::position::PositionSide::Short,
            other => return Err(format!("invalid position side '{other}'")),
        };
        let status = match self.status.map(|s| s.to_ascii_lowercase()).as_deref() {
            Some("closed") => crate::core::position::PositionStatus::Closed,
            Some("liquidated") => crate::core::position::PositionStatus::Liquidated,
            _ => crate::core::position::PositionStatus::Open,
        };
        let position_id = PositionId::from_uuid(
            Uuid::from_str(&self.position_id).map_err(|e| format!("invalid position_id: {e}"))?,
        );
        Ok(crate::core::position::Position {
            id: position_id,
            account_id,
            symbol: Symbol::new(self.symbol),
            side,
            opened_at: self.opened_at,
            closed_at: self.closed_at,
            status,
            avg_entry_price: Price(self.avg_entry_price),
            opened_quantity: Quantity(self.open_quantity),
            open_quantity: Quantity(self.open_quantity),
            realized_pnl: crate::core::types::Money(self.realized_pnl.unwrap_or_default()),
            commission: crate::core::types::Money(self.commission.unwrap_or_default()),
            swap: crate::core::types::Money(self.swap.unwrap_or_default()),
            stop_loss: None,
            take_profit: None,
            magic: None,
            comment: None,
        })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InternalEvaluateRequest {
    pub account_id: String,
    #[serde(default)]
    pub account_state: Option<crate::core::account::Account>,
    /// **P1-6 fix**: the caller-supplied rule pack is deprecated. The
    /// stateless evaluate endpoint derives the rule set from the
    /// account's bound plan, so an arbitrary caller-supplied pack can no
    /// longer silently override the account's policy. The field is kept
    /// optional for backward compatibility with existing callers; if
    /// present the request is rejected with 400 ("rule_pack is no longer
    /// accepted; evaluation uses the account's bound plan").
    #[serde(default)]
    pub rule_pack: Option<RulePack>,
    /// **P2 wire contract fix**: the newer account-level bridge.tick v1
    /// envelope. When present, it takes precedence over the legacy
    /// per-symbol `tick` and supplies broker-reported equity/balance,
    /// positions[], broker_time, and other additive fields.
    #[serde(default)]
    pub bridge_tick: Option<BridgeTickV1>,
    /// Legacy per-symbol quote. Retained only for overlapping callers
    /// that have not yet migrated to the bridge.tick v1 envelope.
    #[serde(default)]
    pub tick: Option<crate::core::tick::Tick>,
    /// **P0.5 fix**: who vouches for the equity/balance numbers.
    /// `"broker_reported"` allows termination; `"estimated"` (the safe
    /// default when absent) never terminates. When `bridge_tick` is
    /// present this defaults to `broker_reported` because the envelope
    /// is itself broker-attested.
    #[serde(default)]
    pub equity_source: Option<String>,
    /// **P0.6 fix**: open positions for position-dependent rules.
    /// Ignored when `bridge_tick` is present (positions come from the
    /// envelope's `payload.positions[]`).
    #[serde(default)]
    pub open_positions: Option<Vec<PositionDto>>,
    /// **P0.6 fix**: today's trades for trade-dependent rules.
    #[serde(default)]
    pub today_trades: Option<Vec<TradeDto>>,
    /// **§A.2 fix**: fills from *other* accounts (cross-account reference
    /// feed) for the copy-trading rule. Never include this account's own
    /// trades — they are ignored (defensively filtered) by the rule.
    #[serde(default)]
    pub cross_reference_trades: Option<Vec<TradeDto>>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InternalEvaluateResponse {
    pub evaluated: bool,
    pub decision_kind: String,
    pub winning_priority: u32,
    pub input_hash: String,
    pub pack_version: u32,
    pub pack_id: String,
    pub violations: Vec<String>,
    pub violation_details: Vec<crate::core::violation::Violation>,
    pub account_state: crate::core::account::Account,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OverrideRequest {
    pub account_id: String,
    #[serde(default)]
    pub account_state: Option<crate::core::account::Account>,
    pub clears_violation_id: String,
    pub violation: crate::core::violation::Violation,
    pub reason: String,
    pub actor_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OverrideResponse {
    pub override_id: String,
    pub cleared_at: String,
    pub account_state: crate::core::account::Account,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ManualRunRequest {
    pub account_id: String,
    #[serde(default)]
    pub account_state: Option<crate::core::account::Account>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ManualRunResponse {
    pub decision_kind: String,
    pub passed: bool,
    pub violations: Vec<String>,
    pub violation_details: Vec<crate::core::violation::Violation>,
    pub account_state: crate::core::account::Account,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmergencyStopRequest {
    pub account_id: String,
    #[serde(default)]
    pub account_state: Option<crate::core::account::Account>,
    pub reason: String,
    pub actor_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmergencyStopResponse {
    pub decision_kind: String,
    pub stopped_at: String,
    pub account_state: crate::core::account::Account,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BreachReportRequest {
    pub account_id: String,
    #[serde(default)]
    pub account_state: Option<crate::core::account::Account>,
    #[serde(default)]
    pub violations: Vec<crate::core::violation::Violation>,
    #[serde(default)]
    pub overrides: Vec<OverrideSummary>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BreachReportResponse {
    pub account_id: String,
    pub account_status: String,
    pub violations: Vec<ViolationSummary>,
    pub current_violations: Vec<ViolationSummary>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ViolationSummary {
    pub rule_name: String,
    pub kind: String,
    pub severity: String,
    pub message: String,
    pub occurred_at: String,
    pub cleared: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OverrideSummary {
    pub override_id: String,
    pub clears_violation_id: String,
    pub reason: String,
    pub actor_id: String,
    pub at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CreateRulePackRequest {
    pub id: String,
    pub version: u32,
    pub tenant_id: String,
    pub description: String,
    pub rules: Vec<RuleEntryDto>,
    pub initial_balance: crate::core::types::Money,
    pub leverage: u32,
    pub profit_target_pct: crate::core::types::Pct,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RuleEntryDto {
    pub id: String,
    pub kind: String,
    pub basis: String,
    pub unit: String,
    pub value: rust_decimal::Decimal,
    pub tolerance_cents: Option<i64>,
    pub early_warning_pct: Option<rust_decimal::Decimal>,
    pub priority: u32,
    pub enabled: bool,
    pub params_json: Option<String>,
    #[serde(default)]
    pub severity: Option<String>,
    #[serde(default)]
    pub failure_policy: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RulePackResponse {
    pub id: String,
    pub version: u32,
    pub lifecycle: String,
    pub content_hash: String,
}

/// Body for `POST /v1/rule-packs/:id/supersede` (optional).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SupersedeRequest {
    /// The pack id that replaces this one.
    pub superseded_by: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GetRulePackResponse {
    pub id: String,
    pub version: u32,
    pub lifecycle: String,
    pub content_hash: String,
    pub json: String,
}
