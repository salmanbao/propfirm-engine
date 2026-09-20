//! HTTP handlers.

use crate::api::dto::{AccountSnapshotDto, EvaluateOrderRequest, EvaluateOrderResponse};
use crate::core::ids::AccountId;
use crate::core::order::{Order, OrderKind, OrderSide, OrderType, TimeInForce};
use crate::core::types::{Price, Quantity, Symbol};
use crate::engine::pipeline::{Pipeline, PipelineEvent};
use crate::notifications::log::LogNotifier;
use crate::persistence::memory::InMemoryStore;
use crate::persistence::traits::AccountStore;
use std::str::FromStr;
use std::sync::Arc;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use parking_lot::RwLock;
use uuid::Uuid;

pub type SharedState = Arc<RwLock<crate::api::server::ServerState>>;

pub async fn health() -> &'static str {
    "ok"
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

#[allow(dead_code)]
pub fn build_pipeline(state: &crate::api::server::ServerState) -> Pipeline<InMemoryStore, LogNotifier> {
    Pipeline::new(state.evaluator.clone(), state.store.clone(), state.notifier.clone())
}
