//! Data transfer objects for the HTTP API.

use crate::core::types::{Money, Pct};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EvaluateOrderRequest {
    pub account_id: String,
    #[serde(default)]
    pub account_state: Option<crate::core::account::Account>,
    pub symbol: String,
    pub side: String,
    pub quantity: rust_decimal::Decimal,
    pub order_type: String,
    pub price: Option<rust_decimal::Decimal>,
    pub stop_loss: Option<rust_decimal::Decimal>,
    pub take_profit: Option<rust_decimal::Decimal>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EvaluateOrderResponse {
    pub decision: String,
    pub passed: bool,
    pub violations: Vec<String>,
    pub violation_details: Vec<crate::core::violation::Violation>,
    pub account_state: crate::core::account::Account,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AccountSnapshotDto {
    pub balance: Money,
    pub equity: Money,
    pub net_profit: Money,
    pub daily_drawdown: Money,
    pub total_drawdown: Money,
    pub daily_dd_utilization: Pct,
    pub max_dd_utilization: Pct,
    pub profit_target_utilization: Pct,
    pub active_trading_days: u32,
    pub trading_day_index: u32,
}

impl From<&crate::core::account::AccountSnapshot> for AccountSnapshotDto {
    fn from(s: &crate::core::account::AccountSnapshot) -> Self {
        AccountSnapshotDto {
            balance: s.balance,
            equity: s.equity,
            net_profit: s.net_profit,
            daily_drawdown: s.daily_drawdown,
            total_drawdown: s.total_drawdown,
            daily_dd_utilization: s.daily_dd_utilization,
            max_dd_utilization: s.max_dd_utilization,
            profit_target_utilization: s.profit_target_utilization,
            active_trading_days: s.active_trading_days,
            trading_day_index: s.trading_day_index,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CreateAccountRequest {
    pub account_id: String,
    pub tenant_id: String,
    pub plan_id: String,
    pub initial_balance: Option<Money>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CreateAccountResponse {
    pub account_id: String,
    pub tenant_id: String,
    pub status: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OverrideRequest {
    pub account_id: String,
    #[serde(default)]
    pub account_state: Option<crate::core::account::Account>,
    pub clears_violation_id: String,
    pub reason: String,
    pub actor_id: String,
    #[serde(default)]
    pub violation: Option<crate::core::violation::Violation>,
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
}
