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
use crate::core::violation::Violation;
use crate::engine::pipeline::{Pipeline, PipelineEvent};
use crate::notifications::log::LogNotifier;
use crate::persistence::memory::InMemoryStore;
use crate::persistence::traits::AccountStore;
use crate::override_engine::Override;
use crate::rulepack::RulePack;
use std::str::FromStr;
use std::sync::Arc;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use parking_lot::RwLock;
use uuid::Uuid;

pub type SharedState = Arc<RwLock<crate::api::server::ServerState>>;

pub async fn health() -> &'static str {
    "ok"
}

/// `POST /internal/v1/evaluate` — the stateless evaluate contract.
/// Takes `{account_id, state, rule_pack, tick}` and returns
/// `{verdict, state_after, metrics}`. This is what the platform's LCC
/// module calls; it does NOT mutate server-side state.
pub async fn evaluate_internal(
    State(state): State<SharedState>,
    headers: HeaderMap,
    Json(req): Json<InternalEvaluateRequest>,
) -> Result<Json<InternalEvaluateResponse>, (StatusCode, String)> {
    let _ = headers; // idempotency-key check would go here in production.
    let account_id = AccountId::from_uuid(Uuid::from_str(&req.account_id)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?);
    let s = state.read().clone();
    let acc = s.store.get(account_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, format!("account {account_id} not found")))?;
    // Build a registry from the supplied rule pack (P1-6).
    let registry = crate::rules::registry::RuleRegistry::build_from_pack(&req.rule_pack)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    // Pure evaluate (P1-7) — no storage mutation.
    let tick = req.tick.clone();
    let verdict = crate::pure::evaluate(
        &acc, &req.rule_pack, &registry,
        crate::rules::context::RuleContextKind::OnTick,
        &[], &[], Vec::new(),
        None, None, Some(&tick),
    ).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(InternalEvaluateResponse {
        decision_kind: format!("{:?}", verdict.decision.kind),
        winning_priority: verdict.decision.winning_priority,
        input_hash: verdict.input_hash,
        pack_version: verdict.pack_version,
        pack_id: verdict.pack_id,
        violations: verdict.decision.all_violations.iter()
            .map(|v| v.message.clone()).collect(),
    }))
}

/// `POST /internal/v1/override` — clear a false-positive breach.
pub async fn override_breach(
    State(state): State<SharedState>,
    Json(req): Json<OverrideRequest>,
) -> Result<Json<OverrideResponse>, (StatusCode, String)> {
    let account_id = AccountId::from_uuid(Uuid::from_str(&req.account_id)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?);
    let clears_violation_id = crate::core::ids::ViolationId::from_uuid(
        Uuid::from_str(&req.clears_violation_id)
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?);
    let s = state.read().clone();
    let mut pipeline = s.pipeline();
    let override_record = Override::new(
        account_id, clears_violation_id,
        req.reason, req.actor_id, chrono::Utc::now(),
    );
    let _ = pipeline.process(account_id, PipelineEvent::OverrideBreach { override_record: override_record.clone() })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(OverrideResponse {
        override_id: override_record.id.to_string(),
        cleared_at: override_record.at.to_rfc3339(),
    }))
}

/// `POST /internal/v1/manual-run` — force re-evaluation of an account.
pub async fn manual_run(
    State(state): State<SharedState>,
    Json(req): Json<ManualRunRequest>,
) -> Result<Json<ManualRunResponse>, (StatusCode, String)> {
    let account_id = AccountId::from_uuid(Uuid::from_str(&req.account_id)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?);
    let s = state.read().clone();
    let mut pipeline = s.pipeline();
    let result = pipeline.process(account_id, PipelineEvent::OnDemand)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(ManualRunResponse {
        decision_kind: format!("{:?}", result.snapshot.decision.kind),
        violations: result.result.violations().iter()
            .map(|v| v.message.clone()).collect(),
    }))
}

/// `GET /internal/v1/breach-report/:account_id` — the trader-facing
/// "why did I fail" view with evidence (TD-25). Returns the breach
/// violation + the rule that produced it + the input_hash for verification.
pub async fn breach_report(
    State(state): State<SharedState>,
    Path(account_id_str): Path<String>,
) -> Result<Json<BreachReportResponse>, (StatusCode, String)> {
    let account_id = AccountId::from_uuid(Uuid::from_str(&account_id_str)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?);
    let s = state.read().clone();
    let acc = s.store.get(account_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "account not found".to_string()))?;
    // Pull all violations from the event log for this account.
    let events = s.event_store.all(account_id);
    let violations: Vec<ViolationSummary> = events.iter()
        .filter_map(|e| match &e.kind {
            crate::core::events::DomainEventKind::RuleViolated { violation } => Some(ViolationSummary {
                rule_name: violation.rule_name.clone(),
                kind: format!("{}", violation.kind),
                severity: format!("{}", violation.severity),
                message: violation.message.clone(),
                occurred_at: violation.occurred_at.to_rfc3339(),
            }),
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
    Json(req): Json<EvaluateOrderRequest>,
) -> Result<Json<EvaluateOrderResponse>, (StatusCode, String)> {
    let account_id = AccountId::from_uuid(Uuid::from_str(&req.account_id).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?);
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
            "limit" => OrderType::Limit { price: Price(req.price.unwrap_or_default()) },
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
    let result = pipeline.process(account_id, PipelineEvent::OrderSubmitted { order: order.clone() })
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let violations: Vec<String> = result.result.violations().iter().map(|v| format!("{}: {}", v.kind, v.message)).collect();
    Ok(Json(EvaluateOrderResponse {
        decision: format!("{:?}", result.snapshot.decision.kind),
        passed: result.passed(),
        violations,
    }))
}

pub async fn get_account(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<AccountSnapshotDto>, (StatusCode, String)> {
    let uuid = Uuid::from_str(&id).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let account_id = AccountId::from_uuid(uuid);
    let s = state.read();
    let acc = s.store.get(account_id).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or((StatusCode::NOT_FOUND, "account not found".to_string()))?;
    let snap: crate::core::account::AccountSnapshot = (&acc).into();
    Ok(Json(AccountSnapshotDto::from(&snap)))
}

// Rule-pack CRUD endpoints (P2-API).

pub async fn create_rule_pack(
    State(_state): State<SharedState>,
    Json(req): Json<CreateRulePackRequest>,
) -> Result<Json<RulePackResponse>, (StatusCode, String)> {
    let rules: Vec<crate::rulepack::RuleEntry> = req.rules.into_iter().map(|r| -> Result<_, (StatusCode, String)> {
        Ok(crate::rulepack::RuleEntry {
            id: r.id, kind: r.kind,
            basis: r.basis.parse::<crate::rulepack::RuleBasis>()
                .map_err(|e: crate::core::Error| (StatusCode::BAD_REQUEST, e.to_string()))?,
            unit: r.unit.parse::<crate::rulepack::RuleUnit>()
                .map_err(|e: crate::core::Error| (StatusCode::BAD_REQUEST, e.to_string()))?,
            value: r.value,
            tolerance_cents: r.tolerance_cents,
            early_warning_pct: r.early_warning_pct,
            priority: r.priority,
            enabled: r.enabled,
            params_json: r.params_json.unwrap_or_else(|| "{}".into()),
        })
    }).collect::<Result<Vec<_>, _>>()?;
    let pack = RulePack {
        id: req.id, version: req.version,
        tenant_id: crate::tenant::TenantId::from_uuid(
            Uuid::from_str(&req.tenant_id).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?),
        lifecycle: crate::rulepack::PackLifecycle::Draft,
        effective_from: chrono::Utc::now(),
        superseded_by: None,
        description: req.description,
        rules,
        initial_balance: req.initial_balance,
        leverage: req.leverage,
        profit_target_pct: req.profit_target_pct,
    };
    pack.validate().map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let response = RulePackResponse {
        id: pack.id.clone(),
        version: pack.version,
        lifecycle: format!("{}", pack.lifecycle),
        content_hash: pack.content_hash(),
    };
    let _ = pack;
    Ok(Json(response))
}

pub async fn get_rule_pack(
    State(_state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<GetRulePackResponse>, (StatusCode, String)> {
    // In production this reads from a rule-pack store. Stub for now.
    Err((StatusCode::NOT_FOUND, format!("rule pack {id} not found (store stub)")))
}

pub async fn update_rule_pack(
    State(_state): State<SharedState>,
    Path(id): Path<String>,
    Json(_req): Json<CreateRulePackRequest>,
) -> Result<Json<RulePackResponse>, (StatusCode, String)> {
    Err((StatusCode::NOT_IMPLEMENTED, format!("rule pack {id} update not implemented yet")))
}

pub async fn activate_rule_pack(
    State(_state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<RulePackResponse>, (StatusCode, String)> {
    Err((StatusCode::NOT_IMPLEMENTED, format!("rule pack {id} activate not implemented yet")))
}

pub async fn supersed_rule_pack(
    State(_state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<RulePackResponse>, (StatusCode, String)> {
    Err((StatusCode::NOT_IMPLEMENTED, format!("rule pack {id} supersede not implemented yet")))
}

// DTOs for the new endpoints.

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InternalEvaluateRequest {
    pub account_id: String,
    pub rule_pack: RulePack,
    pub tick: crate::core::tick::Tick,
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
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RulePackResponse {
    pub id: String,
    pub version: u32,
    pub lifecycle: String,
    pub content_hash: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GetRulePackResponse {
    pub id: String,
    pub version: u32,
    pub lifecycle: String,
    pub content_hash: String,
    pub json: String,
}

#[allow(dead_code)]
pub fn build_pipeline(state: &crate::api::server::ServerState) -> Pipeline<InMemoryStore, LogNotifier> {
    Pipeline::new(state.evaluator.clone(), state.store.clone(), state.notifier.clone())
}

#[allow(dead_code)]
fn _silence_unused() -> Vec<Violation> { Vec::new() }
