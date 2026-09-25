//! Liquidation action layer (P1.10 fix).
//!
//! When `DecisionKind::Liquidate` or `DecisionKind::Emergency` fires,
//! the engine must produce an actionable instruction for the broker
//! bridge — not just flip the account status. Previously, the engine
//! would emit `Failed` but nothing told the bridge *what* to close.
//!
//! This module defines the [`LiquidationInstruction`] type, which
//! carries the list of open positions to close + the reason + audit
//! metadata. The pipeline emits it as a `DomainEvent::LiquidationRequested`
//! when a Liquidate/Emergency verdict is produced.

use crate::core::ids::{AccountId, PositionId, ViolationId};
use crate::core::position::PositionSide;
use crate::core::types::{Money, Quantity, Symbol, Timestamp};
use crate::core::violation::ViolationKind;

/// A single position to be liquidated. Carries the minimum data the
/// bridge needs to issue a close-market order on the broker side.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serialization", derive(serde::Serialize, serde::Deserialize))]
pub struct LiquidationPosition {
    pub position_id: PositionId,
    pub symbol: Symbol,
    pub side: PositionSide,
    pub open_quantity: Quantity,
    pub avg_entry_price: crate::core::types::Price,
}

impl LiquidationPosition {
    #[must_use]
    pub fn from_position(p: &crate::core::position::Position) -> Self {
        LiquidationPosition {
            position_id: p.id,
            symbol: p.symbol.clone(),
            side: p.side,
            open_quantity: p.open_quantity,
            avg_entry_price: p.avg_entry_price,
        }
    }
}

/// The reason the engine requested liquidation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serialization", derive(serde::Serialize, serde::Deserialize))]
pub enum LiquidationReason {
    /// A rule produced `DecisionKind::Liquidate` (e.g. max drawdown
    /// breach with broker-reported equity).
    RuleBreach(ViolationKind),
    /// An ops/compliance actor triggered `PipelineEvent::EmergencyStop`.
    EmergencyStop,
    /// Manual liquidation by tenant risk staff (e.g. via a
    /// future `PipelineEvent::ManualLiquidate`).
    Manual,
}

impl std::fmt::Display for LiquidationReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LiquidationReason::RuleBreach(k) => write!(f, "rule_breach:{k}"),
            LiquidationReason::EmergencyStop => write!(f, "emergency_stop"),
            LiquidationReason::Manual => write!(f, "manual"),
        }
    }
}

/// Actionable instruction to the broker bridge: close all listed
/// positions immediately. The bridge is expected to acknowledge with
/// `TradeFilled` events for each position (P1.10 + bridge integration).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serialization", derive(serde::Serialize, serde::Deserialize))]
pub struct LiquidationInstruction {
    /// Unique id of this instruction (used by the bridge to dedupe).
    pub id: crate::core::ids::EventId,
    /// Account to liquidate.
    pub account_id: AccountId,
    /// Tenant this account belongs to (P1-9 isolation).
    pub tenant_id: crate::tenant::TenantId,
    /// Positions to close (all of them, full quantity).
    pub positions: Vec<LiquidationPosition>,
    /// Why the liquidation was requested.
    pub reason: LiquidationReason,
    /// The original violation that triggered this (if `RuleBreach`).
    /// Used by the breach-report endpoint to link instruction → violation.
    pub triggered_by_violation_id: Option<ViolationId>,
    /// Estimated total notional being liquidated (display only).
    pub estimated_notional: Money,
    /// When the instruction was issued.
    pub issued_at: Timestamp,
    /// Actor that triggered the liquidation (for audit). For
    /// `EmergencyStop`, this is the `actor_id` from the
    /// `PipelineEvent::EmergencyStop` payload. For `RuleBreach`, this
    /// is "`rule_engine`". For `Manual`, the ops user id.
    pub actor_id: String,
}

impl LiquidationInstruction {
    /// Constructs a new liquidation instruction from the open positions
    /// on an account.
    pub fn new(
        account_id: AccountId,
        tenant_id: crate::tenant::TenantId,
        positions: &[crate::core::position::Position],
        reason: LiquidationReason,
        triggered_by_violation_id: Option<ViolationId>,
        actor_id: impl Into<String>,
        issued_at: Timestamp,
    ) -> Self {
        let liq_positions: Vec<LiquidationPosition> = positions
            .iter()
            .filter(|p| p.is_open())
            .map(LiquidationPosition::from_position)
            .collect();
        // **§C.1 fix**: real notional via the instrument registry —
        // `Σ lots × contract_size × price` per position. Positions whose
        // symbol is unregistered use the 1-unit-per-lot fallback spec,
        // which reduces to the old units×price estimate for them.
        let registry = crate::core::instrument::InstrumentRegistry::new();
        let est_notional: rust_decimal::Decimal = liq_positions
            .iter()
            .map(|p| {
                let spec = registry.get(&p.symbol);
                spec.notional(
                    crate::core::types::Lots(spec.units_to_lots(p.open_quantity).0),
                    p.avg_entry_price,
                )
                .0
                .abs()
            })
            .sum();
        LiquidationInstruction {
            id: crate::core::ids::EventId::new(),
            account_id,
            tenant_id,
            positions: liq_positions,
            reason,
            triggered_by_violation_id,
            estimated_notional: Money(est_notional),
            issued_at,
            actor_id: actor_id.into(),
        }
    }

    /// Returns true if there are no positions to liquidate.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// Returns the number of positions to liquidate.
    #[must_use]
    pub fn len(&self) -> usize {
        self.positions.len()
    }
}
