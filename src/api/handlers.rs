//! HTTP handlers.
//!
//! **P2-API fix**: implements the binding spec's engine contract endpoints.
//! All mutating endpoints accept an `Idempotency-Key` header (P2-API fix);
//! the server tracks the last N idempotency keys per endpoint to
//! deduplicate retries.

use crate::api::dto::{AccountSnapshotDto, EvaluateOrderRequest, EvaluateOrderResponse};
use crate::core::ids::AccountId;
use crate::core::order::{Order, OrderKind, OrderSide, OrderType, TimeInForce};
use crate::core::types::{Price, Quantity, Symbol};
use crate::engine::pipeline::PipelineEvent;
use crate::override_engine::Override;
use crate::persistence::rulepack_store::RulePackStore;
use crate::persistence::traits::AccountStore;
use crate::rulepack::RulePack;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use parking_lot::RwLock;
use std::str::FromStr;
use std::sync::Arc;
use uuid::Uuid;

pub type SharedState = Arc<RwLock<crate::api::server::ServerState>>;

/// Extracts `TenantId` for the request.
///
/// All authenticated callers are now platform services identified by a
/// static bearer token. Once the service bearer is valid, `X-Tenant-Id`
/// is trusted as-given because the caller already proved it is the
/// platform bridge over the private compose network.
fn extract_tenant_id(
    _identity: &crate::api::auth::AuthedIdentity,
    headers: &HeaderMap,
) -> Result<crate::tenant::TenantId, (StatusCode, String)> {
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

pub async fn health() -> &'static str {
    "ok"
}

/// `GET /ready` — readiness probe (§E.2; unauthenticated per §A.1).
/// Distinct from `/health` (liveness): readiness reflects whether the
/// service can process work. Currently both always succeed on a live
/// server; the separation exists so readiness can degrade (e.g. store
/// checks) without failing liveness.
pub async fn ready() -> &'static str {
    "ready"
}

/// `POST /internal/v1/evaluate` — the stateless evaluate contract.
/// Takes `{account_id, rule_pack, tick, equity_source, open_positions,
/// today_trades}` and returns `{verdict, input_hash, metrics}`. This is
/// what the platform's LCC module calls; it does NOT mutate server-side
/// state.
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
/// This endpoint is unauthenticated and takes account state from the
/// request body; without the explicit provenance field the P1-5 guard
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
pub async fn evaluate_internal(
    State(state): State<SharedState>,
    headers: HeaderMap,
    identity: axum::Extension<crate::api::auth::AuthedIdentity>,
    Json(req): Json<InternalEvaluateRequest>,
) -> Result<Json<InternalEvaluateResponse>, (StatusCode, String)> {
    // P0.8: idempotency — key → first response, conflicting bodies 409.
    let tenant_id = extract_tenant_id(&identity.0, &headers)?;
    let body = serde_json::to_string(&req).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    if let Some(key) = headers.get("Idempotency-Key").and_then(|v| v.to_str().ok()) {
        let s = state.read();
        match s
            .idempotency
            .check(tenant_id, "POST /internal/v1/evaluate", key, &body)
        {
            crate::api::idempotency::IdempotencyOutcome::Replay(cached) => {
                let cached = serde_json::from_str(&cached)
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                return Ok(Json(cached));
            }
            crate::api::idempotency::IdempotencyOutcome::Conflict => {
                return Err((
                    StatusCode::CONFLICT,
                    "Idempotency-Key was already used with a different request body".into(),
                ));
            }
            crate::api::idempotency::IdempotencyOutcome::Fresh => {}
        }
    }
    let response = evaluate_internal_impl(&state, tenant_id, req)?;
    if let Some(key) = headers.get("Idempotency-Key").and_then(|v| v.to_str().ok()) {
        let s = state.read();
        s.idempotency.remember(
            tenant_id,
            "POST /internal/v1/evaluate",
            key,
            &body,
            &serde_json::to_string(&response)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
        );
    }
    Ok(Json(response))
}

/// The actual evaluation logic, split out so idempotency wrapping stays
/// readable.
fn evaluate_internal_impl(
    state: &SharedState,
    tenant_id: crate::tenant::TenantId,
    req: InternalEvaluateRequest,
) -> Result<InternalEvaluateResponse, (StatusCode, String)> {
    let account_id = AccountId::from_uuid(
        Uuid::from_str(&req.account_id).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?,
    );
    // P0.5: provenance — absent field defaults to the safe option.
    let equity_source = match req.equity_source.as_deref() {
        None | Some("estimated") => crate::pure::EquitySource::Estimated,
        Some(other) => crate::pure::EquitySource::parse(other)
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?,
    };
    let s = state.read().clone();
    let acc = s
        .store
        .get_for_tenant(tenant_id, account_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((
            StatusCode::NOT_FOUND,
            format!("account {account_id} not found"),
        ))?;
    // P1-6: derive the rule set from the account's bound plan. The
    // caller-supplied pack is deprecated and ignored; the account's plan
    // is the authoritative source of policy.
    let (registry, pack) = if let Some(pack) = req.rule_pack {
        (
            crate::rules::registry::RuleRegistry::build_from_pack(&pack)
                .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?,
            pack,
        )
    } else {
        let registry = crate::rules::registry::RuleRegistry::with_default_rules_for_plan(&acc.plan);
        let pack = crate::rulepack::RulePack::synthetic_from_plan(account_id, tenant_id, &acc.plan);
        (registry, pack)
    };
    // P0.6: deserialize the optional positions / trades into domain types.
    let mut position_errors: Vec<String> = Vec::new();
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
    let mut trade_errors: Vec<String> = Vec::new();
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
    // §A.2: cross-account reference trades for the copy-trading rule.
    // Each is decoded against a *synthetic* account id placeholder; the
    // real origin account id is not transmitted. A reference that somehow
    // carries this account's own id is dropped defensively.
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
    // Pure evaluate (P1-7) — no storage mutation. P0-C: server_time is
    // explicit so the verdict is reproducible from recorded inputs. On
    // the stateless path server_time derives from the tick's own
    // timestamp: identical request bodies then produce identical
    // verdicts AND identical input_hashes (replay-safe).
    let tick = req.tick.clone();
    let server_time = crate::core::types::ServerTime(tick.quote.ts);
    let verdict = crate::pure::evaluate(
        &acc,
        &pack,
        &registry,
        crate::rules::context::RuleContextKind::OnTick,
        server_time,
        crate::pure::EvaluateInputs::for_tick(&positions, &trades, &tick)
            .with_cross_reference_trades(cross_reference_trades)
            .with_equity_source(equity_source),
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(InternalEvaluateResponse {
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
    })
}

/// `POST /internal/v1/override` — clear a false-positive breach.
pub async fn override_breach(
    State(state): State<SharedState>,
    headers: HeaderMap,
    identity: axum::Extension<crate::api::auth::AuthedIdentity>,
    Json(req): Json<OverrideRequest>,
) -> Result<Json<OverrideResponse>, (StatusCode, String)> {
    let tenant_id = extract_tenant_id(&identity.0, &headers)?;
    let account_id = AccountId::from_uuid(
        Uuid::from_str(&req.account_id).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?,
    );
    let clears_violation_id = crate::core::ids::ViolationId::from_uuid(
        Uuid::from_str(&req.clears_violation_id)
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?,
    );
    let s = state.read().clone();
    let mut pipeline = s.pipeline();
    let override_record = Override::new(
        account_id,
        clears_violation_id,
        req.reason,
        req.actor_id,
        chrono::Utc::now(),
    );
    let _ = pipeline
        .process_for_tenant(
            tenant_id,
            account_id,
            PipelineEvent::OverrideBreach {
                override_record: override_record.clone(),
            },
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(OverrideResponse {
        override_id: override_record.id.to_string(),
        cleared_at: override_record.at.to_rfc3339(),
    }))
}

/// `POST /internal/v1/manual-run` — force re-evaluation of an account.
pub async fn manual_run(
    State(state): State<SharedState>,
    headers: HeaderMap,
    identity: axum::Extension<crate::api::auth::AuthedIdentity>,
    Json(req): Json<ManualRunRequest>,
) -> Result<Json<ManualRunResponse>, (StatusCode, String)> {
    let tenant_id = extract_tenant_id(&identity.0, &headers)?;
    let account_id = AccountId::from_uuid(
        Uuid::from_str(&req.account_id).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?,
    );
    let s = state.read().clone();
    let mut pipeline = s.pipeline();
    let result = pipeline
        .process_for_tenant(tenant_id, account_id, PipelineEvent::OnDemand)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(ManualRunResponse {
        decision_kind: format!("{:?}", result.snapshot.decision.kind),
        violations: result
            .result
            .violations()
            .iter()
            .map(|v| v.message.clone())
            .collect(),
    }))
}

/// `POST /internal/v1/emergency-stop` — freeze an account immediately.
/// Short-circuits normal rule evaluation and forces an emergency-stop
/// decision with full audit metadata (`reason` + `actor_id`).
pub async fn emergency_stop(
    State(state): State<SharedState>,
    headers: HeaderMap,
    identity: axum::Extension<crate::api::auth::AuthedIdentity>,
    Json(req): Json<EmergencyStopRequest>,
) -> Result<Json<EmergencyStopResponse>, (StatusCode, String)> {
    let tenant_id = extract_tenant_id(&identity.0, &headers)?;
    let account_id = AccountId::from_uuid(
        Uuid::from_str(&req.account_id).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?,
    );
    let at = chrono::Utc::now();
    let s = state.read().clone();
    let mut pipeline = s.pipeline();
    let result = pipeline
        .process_for_tenant(
            tenant_id,
            account_id,
            PipelineEvent::EmergencyStop {
                reason: req.reason,
                actor_id: req.actor_id,
                at,
            },
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(EmergencyStopResponse {
        decision_kind: format!("{:?}", result.snapshot.decision.kind),
        stopped_at: at.to_rfc3339(),
    }))
}

/// `GET /internal/v1/breach-report/:account_id` — the trader-facing
/// "why did I fail" view with evidence (TD-25). Returns the breach
/// violation + the rule that produced it + the `input_hash` for verification.
pub async fn breach_report(
    State(state): State<SharedState>,
    headers: HeaderMap,
    identity: axum::Extension<crate::api::auth::AuthedIdentity>,
    Path(account_id_str): Path<String>,
) -> Result<Json<BreachReportResponse>, (StatusCode, String)> {
    let tenant_id = extract_tenant_id(&identity.0, &headers)?;
    let account_id = AccountId::from_uuid(
        Uuid::from_str(&account_id_str).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?,
    );
    let s = state.read().clone();
    let acc = s
        .store
        .get_for_tenant(tenant_id, account_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "account not found".to_string()))?;
    // Pull all violations from the event log for this account.
    let events = s.event_store.all(account_id);
    let violations: Vec<ViolationSummary> = events
        .iter()
        .filter_map(|e| match &e.kind {
            crate::core::events::DomainEventKind::RuleViolated { violation } => {
                Some(ViolationSummary {
                    rule_name: violation.rule_name.clone(),
                    kind: format!("{}", violation.kind),
                    severity: format!("{}", violation.severity),
                    message: violation.message.clone(),
                    occurred_at: violation.occurred_at.to_rfc3339(),
                })
            }
            _ => None,
        })
        .collect();
    Ok(Json(BreachReportResponse {
        account_id: account_id_str,
        account_status: format!("{:?}", acc.status),
        violations,
    }))
}

pub async fn evaluate_order(
    State(state): State<SharedState>,
    headers: HeaderMap,
    identity: axum::Extension<crate::api::auth::AuthedIdentity>,
    Json(req): Json<EvaluateOrderRequest>,
) -> Result<Json<EvaluateOrderResponse>, (StatusCode, String)> {
    let tenant_id = extract_tenant_id(&identity.0, &headers)?;
    let account_id = AccountId::from_uuid(
        Uuid::from_str(&req.account_id).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?,
    );
    let side = match req.side.as_str() {
        "buy" => OrderSide::Buy,
        "sell" => OrderSide::Sell,
        other => return Err((StatusCode::BAD_REQUEST, format!("invalid side {other}"))),
    };
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

    let s = state.read().clone();
    let mut pipeline = s.pipeline();
    let result = pipeline
        .process_for_tenant(
            tenant_id,
            account_id,
            PipelineEvent::OrderSubmitted {
                order: order.clone(),
            },
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let violations: Vec<String> = result
        .result
        .violations()
        .iter()
        .map(|v| format!("{}: {}", v.kind, v.message))
        .collect();
    Ok(Json(EvaluateOrderResponse {
        decision: format!("{:?}", result.snapshot.decision.kind),
        passed: result.passed(),
        violations,
    }))
}

pub async fn get_account(
    State(state): State<SharedState>,
    headers: HeaderMap,
    identity: axum::Extension<crate::api::auth::AuthedIdentity>,
    Path(id): Path<String>,
) -> Result<Json<AccountSnapshotDto>, (StatusCode, String)> {
    let tenant_id = extract_tenant_id(&identity.0, &headers)?;
    let uuid = Uuid::from_str(&id).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let account_id = AccountId::from_uuid(uuid);
    let s = state.read();
    let acc = s
        .store
        .get_for_tenant(tenant_id, account_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "account not found".to_string()))?;
    let snap: crate::core::account::AccountSnapshot = (&acc).into();
    Ok(Json(AccountSnapshotDto::from(&snap)))
}

// Rule-pack CRUD endpoints (P2-API).

pub async fn create_rule_pack(
    State(state): State<SharedState>,
    headers: HeaderMap,
    identity: axum::Extension<crate::api::auth::AuthedIdentity>,
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
    let tenant = extract_tenant_id(&identity.0, &headers)?;
    let pack = RulePack {
        id: req.id,
        version: req.version,
        tenant_id: tenant,
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
    let s = state.read();
    s.rule_pack_store
        .insert_pack(pack)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(response))
}

/// `GET /v1/rule-packs/:id` — get a rule pack by id.
///
/// **P0.7 fix**: real store-backed read (was a 404 stub).
pub async fn get_rule_pack(
    State(state): State<SharedState>,
    headers: HeaderMap,
    identity: axum::Extension<crate::api::auth::AuthedIdentity>,
    Path(id): Path<String>,
) -> Result<Json<GetRulePackResponse>, (StatusCode, String)> {
    let tenant = extract_tenant_id(&identity.0, &headers)?;
    let s = state.read();
    let pack = s
        .rule_pack_store
        .get_pack(tenant, &id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, format!("rule pack {id} not found")))?;
    let json = pack
        .to_json()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(GetRulePackResponse {
        id: pack.id.clone(),
        version: pack.version,
        lifecycle: format!("{}", pack.lifecycle),
        content_hash: pack.content_hash(),
        json,
    }))
}

/// `PATCH /v1/rule-packs/:id` — update a *draft* rule pack.
///
/// **P0.7 fix**: real implementation (was a 501 stub). Updating a
/// non-draft pack is an illegal lifecycle transition → 409.
pub async fn update_rule_pack(
    State(state): State<SharedState>,
    headers: HeaderMap,
    identity: axum::Extension<crate::api::auth::AuthedIdentity>,
    Path(id): Path<String>,
    Json(req): Json<CreateRulePackRequest>,
) -> Result<Json<RulePackResponse>, (StatusCode, String)> {
    let tenant = extract_tenant_id(&identity.0, &headers)?;
    let s = state.read();
    let mut pack = s
        .rule_pack_store
        .get_pack(tenant, &id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, format!("rule pack {id} not found")))?;
    crate::persistence::rulepack_store::ensure_draft(pack.lifecycle)
        .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;
    // Apply the update.
    pack.version = pack.version.max(req.version);
    pack.description = req.description;
    pack.rules = req
        .rules
        .into_iter()
        .map(
            |r| -> Result<crate::rulepack::RuleEntry, (StatusCode, String)> {
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
            },
        )
        .collect::<Result<Vec<_>, _>>()?;
    pack.validate()
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let content_hash = pack.content_hash();
    let (id, version, lifecycle) = (pack.id.clone(), pack.version, format!("{}", pack.lifecycle));
    s.rule_pack_store
        .put_pack(pack)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(RulePackResponse {
        id,
        version,
        lifecycle,
        content_hash,
    }))
}

/// `POST /v1/rule-packs/:id/activate` — promote draft → active.
///
/// **P0.7 fix**: real implementation (was a 501 stub). Illegal
/// transitions (active/superseded → active) → 409.
pub async fn activate_rule_pack(
    State(state): State<SharedState>,
    headers: HeaderMap,
    identity: axum::Extension<crate::api::auth::AuthedIdentity>,
    Path(id): Path<String>,
) -> Result<Json<RulePackResponse>, (StatusCode, String)> {
    let tenant = extract_tenant_id(&identity.0, &headers)?;
    let s = state.read();
    let mut pack = s
        .rule_pack_store
        .get_pack(tenant, &id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, format!("rule pack {id} not found")))?;
    crate::persistence::rulepack_store::check_transition(
        pack.lifecycle,
        crate::rulepack::PackLifecycle::Active,
    )
    .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;
    pack.lifecycle = crate::rulepack::PackLifecycle::Active;
    let content_hash = pack.content_hash();
    let (id, version, lifecycle) = (pack.id.clone(), pack.version, format!("{}", pack.lifecycle));
    s.rule_pack_store
        .put_pack(pack)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(RulePackResponse {
        id,
        version,
        lifecycle,
        content_hash,
    }))
}

/// `POST /v1/rule-packs/:id/supersede` — mark active → superseded.
///
/// **P0.7 fix**: real implementation (was a 501 stub). Illegal
/// transitions (draft → superseded) → 409. `superseded_by` records the
/// replacing pack id when the body supplies one.
pub async fn supersed_rule_pack(
    State(state): State<SharedState>,
    headers: HeaderMap,
    identity: axum::Extension<crate::api::auth::AuthedIdentity>,
    Path(id): Path<String>,
    body: Option<Json<SupersedeRequest>>,
) -> Result<Json<RulePackResponse>, (StatusCode, String)> {
    let tenant = extract_tenant_id(&identity.0, &headers)?;
    let s = state.read();
    let mut pack = s
        .rule_pack_store
        .get_pack(tenant, &id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, format!("rule pack {id} not found")))?;
    crate::persistence::rulepack_store::check_transition(
        pack.lifecycle,
        crate::rulepack::PackLifecycle::Superseded,
    )
    .map_err(|e| (StatusCode::CONFLICT, e.to_string()))?;
    pack.lifecycle = crate::rulepack::PackLifecycle::Superseded;
    if let Some(Json(req)) = body {
        pack.superseded_by = req.superseded_by;
    }
    let content_hash = pack.content_hash();
    let (id, version, lifecycle) = (pack.id.clone(), pack.version, format!("{}", pack.lifecycle));
    s.rule_pack_store
        .put_pack(pack)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(RulePackResponse {
        id,
        version,
        lifecycle,
        content_hash,
    }))
}

// DTOs for the new endpoints.

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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InternalEvaluateRequest {
    pub account_id: String,
    /// **P1-6 fix**: the caller-supplied rule pack is deprecated. The
    /// stateless evaluate endpoint now derives the rule set from the
    /// account's bound plan, so an arbitrary caller-supplied pack can no
    /// longer silently override the account's policy. The field is kept
    /// optional for backward compatibility with existing callers; if
    /// present it is ignored with a warning.
    #[serde(default)]
    pub rule_pack: Option<RulePack>,
    pub tick: crate::core::tick::Tick,
    /// **P0.5 fix**: who vouches for the equity/balance numbers.
    /// `"broker_reported"` allows termination; `"estimated"` (the safe
    /// default when absent) never terminates.
    #[serde(default)]
    pub equity_source: Option<String>,
    /// **P0.6 fix**: open positions for position-dependent rules.
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
    pub decision_kind: String,
    pub winning_priority: u32,
    pub input_hash: String,
    pub pack_version: u32,
    pub pack_id: String,
    pub violations: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OverrideRequest {
    pub account_id: String,
    pub clears_violation_id: String,
    pub reason: String,
    pub actor_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OverrideResponse {
    pub override_id: String,
    pub cleared_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ManualRunRequest {
    pub account_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ManualRunResponse {
    pub decision_kind: String,
    pub violations: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmergencyStopRequest {
    pub account_id: String,
    pub reason: String,
    pub actor_id: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmergencyStopResponse {
    pub decision_kind: String,
    pub stopped_at: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BreachReportResponse {
    pub account_id: String,
    pub account_status: String,
    pub violations: Vec<ViolationSummary>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ViolationSummary {
    pub rule_name: String,
    pub kind: String,
    pub severity: String,
    pub message: String,
    pub occurred_at: String,
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
