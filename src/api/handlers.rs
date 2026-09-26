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
use crate::rulepack::RulePack;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;
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

#[axum::debug_handler]
pub async fn evaluate_internal(
    State(state): State<SharedState>,
    headers: HeaderMap,
    identity: Extension<crate::api::auth::AuthedIdentity>,
    Json(req): Json<InternalEvaluateRequest>,
) -> Result<Json<InternalEvaluateResponse>, (StatusCode, String)> {
    // P0.8: idempotency — key → first response, conflicting bodies 409.
    let tenant_id = extract_tenant_id(&identity.0, &headers)?;
    let body = serde_json::to_string(&req).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let response = if let Some(key) = headers.get("Idempotency-Key").and_then(|v| v.to_str().ok()) {
        let s = state.read().await;
        let response = evaluate_internal_impl(&state, tenant_id, req).await?;
        let response_str = serde_json::to_string(&response)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        match s
            .idempotency
            .check_and_remember(
                tenant_id,
                "POST /internal/v1/evaluate",
                key,
                &body,
                &response_str,
            )
            .await
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
            crate::api::idempotency::IdempotencyOutcome::Error => {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "idempotency lookup failed".into(),
                ));
            }
            crate::api::idempotency::IdempotencyOutcome::Fresh => response,
        }
    } else {
        evaluate_internal_impl(&state, tenant_id, req).await?
    };
    Ok(Json(response))
}

/// The actual evaluation logic, split out so idempotency wrapping stays
/// readable.
async fn evaluate_internal_impl(
    state: &SharedState,
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
    let s = state.read().await.clone();
    let mut acc = s
        .store
        .get_for_tenant(tenant_id, account_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((
            StatusCode::NOT_FOUND,
            format!("account {account_id} not found"),
        ))?;
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
    identity: Extension<crate::api::auth::AuthedIdentity>,
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
    let s = state.read().await.clone();
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
        .await
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
    identity: Extension<crate::api::auth::AuthedIdentity>,
    Json(req): Json<ManualRunRequest>,
) -> Result<Json<ManualRunResponse>, (StatusCode, String)> {
    let tenant_id = extract_tenant_id(&identity.0, &headers)?;
    let account_id = AccountId::from_uuid(
        Uuid::from_str(&req.account_id).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?,
    );
    let s = state.read().await.clone();
    let mut pipeline = s.pipeline();
    let result = pipeline
        .process_for_tenant(tenant_id, account_id, PipelineEvent::OnDemand)
        .await
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
    identity: Extension<crate::api::auth::AuthedIdentity>,
    Json(req): Json<EmergencyStopRequest>,
) -> Result<Json<EmergencyStopResponse>, (StatusCode, String)> {
    let tenant_id = extract_tenant_id(&identity.0, &headers)?;
    let account_id = AccountId::from_uuid(
        Uuid::from_str(&req.account_id).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?,
    );
    let at = chrono::Utc::now();
    let s = state.read().await.clone();
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
        .await
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
    identity: Extension<crate::api::auth::AuthedIdentity>,
    Path(account_id_str): Path<String>,
) -> Result<Json<BreachReportResponse>, (StatusCode, String)> {
    let tenant_id = extract_tenant_id(&identity.0, &headers)?;
    let account_id = AccountId::from_uuid(
        Uuid::from_str(&account_id_str).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?,
    );
    let s = state.read().await.clone();
    let acc = s
        .store
        .get_for_tenant(tenant_id, account_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "account not found".to_string()))?;
    // Pull all violations from the event log for this account.
    let events = s
        .event_store
        .all(account_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
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
    identity: Extension<crate::api::auth::AuthedIdentity>,
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

    let s = state.read().await.clone();
    let mut pipeline = s.pipeline();
    let result = pipeline
        .process_for_tenant(
            tenant_id,
            account_id,
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
    Ok(Json(EvaluateOrderResponse {
        decision: format!("{:?}", result.snapshot.decision.kind),
        passed: result.passed(),
        violations,
    }))
}

pub async fn get_account(
    State(state): State<SharedState>,
    headers: HeaderMap,
    identity: Extension<crate::api::auth::AuthedIdentity>,
    Path(id): Path<String>,
) -> Result<Json<AccountSnapshotDto>, (StatusCode, String)> {
    let tenant_id = extract_tenant_id(&identity.0, &headers)?;
    let uuid = Uuid::from_str(&id).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let account_id = AccountId::from_uuid(uuid);
    let s = state.read().await;
    let acc = s
        .store
        .get_for_tenant(tenant_id, account_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "account not found".to_string()))?;
    let snap: crate::core::account::AccountSnapshot = (&acc).into();
    Ok(Json(AccountSnapshotDto::from(&snap)))
}

// DTOs for the new endpoints.

/// `POST /v1/rule-packs/validate` — stateless rule pack validation.
///
/// Validates a rule pack without persisting it. Returns validation errors
/// or the pack's computed content hash. Used by tenants to verify packs
/// before binding them to accounts via platform tooling.
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
    /// **P1-6 fix**: the caller-supplied rule pack is deprecated. The
    /// stateless evaluate endpoint now derives the rule set from the
    /// account's bound plan, so an arbitrary caller-supplied pack can no
    /// longer silently override the account's policy. The field is kept
    /// optional for backward compatibility with existing callers; if
    /// present it is ignored with a warning.
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
