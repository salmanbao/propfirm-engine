//! Trade (fill) domain model.
//!
//! A [`Trade`] represents a single execution event against an order. Trades
//! realize P&L on closes and adjust position state on opens.

use crate::core::ids::{OrderId, PositionId, TradeId};
use crate::core::order::OrderSide;
use crate::core::types::{Money, Price, Quantity, Symbol, Timestamp};

/// Direction of a trade from the perspective of the position it affects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TradeSide {
    /// Trade increases an open position (or opens a new one).
    Entry,
    /// Trade reduces (or fully closes) an open position.
    Exit,
    /// Trade reverses an open position (close + open opposite).
    Reverse,
}

/// Information about an exit, including the position closed and realized P&L.
#[derive(Debug, Clone)]
pub struct TradeExit {
    pub position_id: PositionId,
    pub realized_pnl: Money,
    pub closed_quantity: Quantity,
    pub entry_price: Price,
    pub exit_price: Price,
}

/// Immutable trade / fill record.
#[derive(Debug, Clone)]
pub struct Trade {
    pub id: TradeId,
    pub order_id: OrderId,
    pub account_id: crate::core::ids::AccountId,
    pub symbol: Symbol,
    pub side: OrderSide,
    pub trade_side: TradeSide,
    pub price: Price,
    pub quantity: Quantity,
    pub commission: Money,
    pub swap: Money,
    pub executed_at: Timestamp,
    /// For exits, the position this trade affected.
    pub exit_info: Option<TradeExit>,
    /// Client comment.
    pub comment: Option<String>,
}

impl Trade {
    /// Constructs a new trade (fill).
    #[must_use]
    #[allow(clippy::too_many_arguments)] // broker fill metadata is inherently wide
    pub fn new(
        order_id: OrderId,
        account_id: crate::core::ids::AccountId,
        symbol: Symbol,
        side: OrderSide,
        trade_side: TradeSide,
        price: Price,
        quantity: Quantity,
        commission: Money,
        executed_at: Timestamp,
    ) -> Self {
        Trade {
            id: TradeId::new(),
            order_id,
            account_id,
            symbol,
            side,
            trade_side,
            price,
            quantity,
            commission,
            swap: Money::ZERO,
            executed_at,
            exit_info: None,
            comment: None,
        }
    }

    /// Net P&L contribution of this trade (realized pnl + commission + swap
    /// for exits; for entries, zero or commission only).
    #[must_use]
    pub fn net_pnl(&self) -> Money {
        let gross = self
            .exit_info
            .as_ref()
            .map_or(Money::ZERO, |e| e.realized_pnl);
        Money(gross.0 - self.commission.0 - self.swap.0)
    }
}
