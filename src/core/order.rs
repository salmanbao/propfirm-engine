//! Order domain model.
//!
//! An [`Order`] represents an intent to trade: enter or exit a position at
//! market or at a specified price. Orders are immutable once created; state
//! transitions are modeled via [`OrderStatus`].

use crate::core::ids::OrderId;
use crate::core::types::{dec, Decimal, Price, Quantity, Symbol, Timestamp};
use crate::core::Error;

/// Side of an order/position.
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[serde(rename_all = "snake_case")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderSide {
    /// Buy / long.
    Buy,
    /// Sell / short.
    Sell,
}

impl OrderSide {
    /// Returns the opposite side.
    #[must_use]
    pub fn opposite(self) -> Self {
        match self {
            OrderSide::Buy => OrderSide::Sell,
            OrderSide::Sell => OrderSide::Buy,
        }
    }

    /// Sign multiplier: +1 for buy, -1 for sell.
    #[must_use]
    pub fn sign(self) -> Decimal {
        match self {
            OrderSide::Buy => dec!(1),
            OrderSide::Sell => dec!(-1),
        }
    }
}

impl std::fmt::Display for OrderSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OrderSide::Buy => write!(f, "buy"),
            OrderSide::Sell => write!(f, "sell"),
        }
    }
}

/// Type of order (price logic).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderType {
    /// Market order – fills at the best available price.
    Market,
    /// Limit order – fills at price or better.
    Limit { price: Price },
    /// Stop order – triggers a market order once price crosses stop.
    Stop { stop: Price },
    /// Stop-limit – triggers a limit order at `limit` once `stop` is hit.
    StopLimit { stop: Price, limit: Price },
}

/// Time in force policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimeInForce {
    /// Good-till-cancelled.
    Gtc,
    /// Immediate-or-cancel (partial fills allowed, remainder cancelled).
    Ioc,
    /// Fill-or-kill (all-or-nothing).
    Fok,
    /// Day order (cancelled at session end).
    Day,
}

/// Order kind: open a position or close an existing one.
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[serde(rename_all = "snake_case")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderKind {
    /// Open a new position.
    Open,
    /// Close an existing position (full or partial).
    Close {
        position_id: crate::core::ids::PositionId,
        partial_quantity: Option<Quantity>,
    },
    /// Reverse an existing position (close + open opposite).
    Reverse,
}

/// Status of an order lifecycle.
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[serde(rename_all = "snake_case")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderStatus {
    /// Created but not yet submitted to the matching engine.
    Pending,
    /// Accepted by the matching engine.
    Accepted,
    /// Partially filled (see `filled_quantity`).
    PartiallyFilled,
    /// Fully filled.
    Filled,
    /// Cancelled by user or system.
    Cancelled,
    /// Rejected (e.g. by risk rules).
    Rejected,
    /// Expired (TIF elapsed).
    Expired,
}

impl OrderStatus {
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            OrderStatus::Filled
                | OrderStatus::Cancelled
                | OrderStatus::Rejected
                | OrderStatus::Expired
        )
    }
}

/// Immutable order representation.
#[derive(Debug, Clone)]
pub struct Order {
    /// Unique identifier.
    pub id: OrderId,
    /// Account that owns the order.
    pub account_id: crate::core::ids::AccountId,
    /// Symbol being traded.
    pub symbol: Symbol,
    /// Buy or sell.
    pub side: OrderSide,
    /// Open / close / reverse.
    pub kind: OrderKind,
    /// Order type and pricing.
    pub order_type: OrderType,
    /// Requested volume.
    pub quantity: Quantity,
    /// Time in force.
    pub tif: TimeInForce,
    /// Optional stop-loss price.
    pub stop_loss: Option<Price>,
    /// Optional take-profit price.
    pub take_profit: Option<Price>,
    /// Optional client comment / magic number.
    pub comment: Option<String>,
    /// Submitted timestamp.
    pub submitted_at: Timestamp,
    /// Current lifecycle status.
    pub status: OrderStatus,
    /// Filled quantity so far.
    pub filled_quantity: Quantity,
    /// Average fill price so far.
    pub avg_fill_price: Option<Price>,
}

impl Order {
    /// Builds a market order to open a new position.
    #[must_use]
    pub fn market_open(
        account_id: crate::core::ids::AccountId,
        symbol: Symbol,
        side: OrderSide,
        quantity: Quantity,
        stop_loss: Option<Price>,
        take_profit: Option<Price>,
        submitted_at: Timestamp,
    ) -> Self {
        Order {
            id: OrderId::new(),
            account_id,
            symbol,
            side,
            kind: OrderKind::Open,
            order_type: OrderType::Market,
            quantity,
            tif: TimeInForce::Ioc,
            stop_loss,
            take_profit,
            comment: None,
            submitted_at,
            status: OrderStatus::Pending,
            filled_quantity: Quantity::ZERO,
            avg_fill_price: None,
        }
    }

    /// Builds a market order to close a position (full or partial).
    #[must_use]
    pub fn market_close(
        account_id: crate::core::ids::AccountId,
        position_id: crate::core::ids::PositionId,
        partial_quantity: Option<Quantity>,
        submitted_at: Timestamp,
    ) -> Self {
        Order {
            id: OrderId::new(),
            account_id,
            symbol: Symbol::new("UNKNOWN"), // will be derived from position
            side: OrderSide::Buy,           // will be overridden by caller
            kind: OrderKind::Close {
                position_id,
                partial_quantity,
            },
            order_type: OrderType::Market,
            quantity: partial_quantity.unwrap_or(Quantity::ZERO),
            tif: TimeInForce::Ioc,
            stop_loss: None,
            take_profit: None,
            comment: None,
            submitted_at,
            status: OrderStatus::Pending,
            filled_quantity: Quantity::ZERO,
            avg_fill_price: None,
        }
    }

    /// Returns true if the order requires SL to be set on submission.
    #[must_use]
    pub fn requires_sl(&self) -> bool {
        self.stop_loss.is_none() && self.kind == OrderKind::Open
    }

    /// Returns true if the order requires TP to be set on submission.
    #[must_use]
    pub fn requires_tp(&self) -> bool {
        self.take_profit.is_none() && self.kind == OrderKind::Open
    }

    /// Marks the order as accepted by the matching engine. Returns a new
    /// order (immutable update).
    pub fn accepted(self) -> Result<Order, Error> {
        if self.status != OrderStatus::Pending {
            return Err(Error::InvalidState(format!(
                "order {} is not pending (status = {:?})",
                self.id, self.status
            )));
        }
        Ok(Order {
            status: OrderStatus::Accepted,
            ..self
        })
    }

    /// Marks the order as rejected with a reason.
    pub fn rejected(self, reason: &str) -> Result<Order, Error> {
        if self.status.is_terminal() {
            return Err(Error::InvalidState(format!(
                "order {} already terminal ({:?})",
                self.id, self.status
            )));
        }
        let comment = match self.comment {
            Some(c) => format!("{c} | rejected: {reason}"),
            None => format!("rejected: {reason}"),
        };
        Ok(Order {
            status: OrderStatus::Rejected,
            comment: Some(comment),
            ..self
        })
    }
}
