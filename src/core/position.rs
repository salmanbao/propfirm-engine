//! Position domain model.
//!
//! A [`Position`] aggregates one or more fills in the same symbol and side.
//! Closed positions carry the realized P&L; open positions expose the
//! unrealized P&L function given a market quote.

use crate::core::ids::PositionId;
use crate::core::order::OrderSide;
use crate::core::tick::Quote;
use crate::core::types::{dec, Decimal, Money, Price, Quantity, Symbol, Timestamp};

/// Side of an open position.
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[serde(rename_all = "snake_case")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PositionSide {
    Long,
    Short,
}

impl PositionSide {
    /// Sign multiplier: +1 for long, -1 for short.
    #[must_use]
    pub fn sign(self) -> Decimal {
        match self {
            PositionSide::Long => dec!(1),
            PositionSide::Short => dec!(-1),
        }
    }
    #[must_use]
    pub fn from_order(s: OrderSide) -> Self {
        match s {
            OrderSide::Buy => PositionSide::Long,
            OrderSide::Sell => PositionSide::Short,
        }
    }
}

/// Position lifecycle status.
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[serde(rename_all = "snake_case")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PositionStatus {
    Open,
    Closed,
    Liquidated,
}

/// Immutable position aggregate. A position is created on the first fill of
/// an open-order and mutated (via new instances) as additional fills or
/// partial closes occur.
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[derive(Debug, Clone)]
pub struct Position {
    pub id: PositionId,
    pub account_id: crate::core::ids::AccountId,
    pub symbol: Symbol,
    pub side: PositionSide,
    pub opened_at: Timestamp,
    pub closed_at: Option<Timestamp>,
    pub status: PositionStatus,

    /// Weighted-average entry price.
    pub avg_entry_price: Price,
    /// Total opened volume (gross, not netted).
    pub opened_quantity: Quantity,
    /// Currently remaining open volume.
    pub open_quantity: Quantity,
    /// Cumulative realized P&L from partial closes.
    pub realized_pnl: Money,
    /// Cumulative commissions paid.
    pub commission: Money,
    /// Cumulative swap (financing) paid.
    pub swap: Money,

    /// Attached SL (zero-or-positive distance from entry).
    pub stop_loss: Option<Price>,
    /// Attached TP.
    pub take_profit: Option<Price>,
    /// Magic number / tag from originating EA.
    pub magic: Option<u64>,
    /// Free-form comment.
    pub comment: Option<String>,
}

impl Position {
    /// Constructs a fresh open position from a single fill.
    #[must_use]
    #[allow(clippy::too_many_arguments)] // broker fill metadata is inherently wide
    pub fn open(
        account_id: crate::core::ids::AccountId,
        symbol: Symbol,
        side: PositionSide,
        entry_price: Price,
        quantity: Quantity,
        opened_at: Timestamp,
        commission: Money,
        stop_loss: Option<Price>,
        take_profit: Option<Price>,
        magic: Option<u64>,
        comment: Option<String>,
    ) -> Self {
        Position {
            id: PositionId::new(),
            account_id,
            symbol,
            side,
            opened_at,
            closed_at: None,
            status: PositionStatus::Open,
            avg_entry_price: entry_price,
            opened_quantity: quantity,
            open_quantity: quantity,
            realized_pnl: Money::ZERO,
            commission,
            swap: Money::ZERO,
            stop_loss,
            take_profit,
            magic,
            comment,
        }
    }

    /// Returns true if the position is currently open.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.status == PositionStatus::Open && self.open_quantity.is_positive()
    }

    /// Computes the unrealized P&L given a current market quote.
    ///
    /// `PnL` = (`current_price` - `entry_price`) * sign * volume
    ///
    /// **§C.1 note**: `volume` here is the raw open quantity in units and
    /// the computation implicitly uses `contract_size = 1`. Instrument-
    /// aware valuation lives on
    /// [`InstrumentSpec`](crate::core::instrument::InstrumentSpec)
    /// (units↔lots + notional); estimators that need contract-size-correct
    /// P&L should scale through it. Unregistered symbols behave exactly
    /// as this function does (1 unit = 1 lot).
    #[must_use]
    pub fn unrealized_pnl(&self, quote: &Quote) -> Money {
        if !self.is_open() {
            return Money::ZERO;
        }
        let price = match self.side {
            PositionSide::Long => quote.bid,
            PositionSide::Short => quote.ask,
        };
        unrealized_pnl(self.avg_entry_price, price, self.open_quantity, self.side)
    }
}

/// Pure helper: computes unrealized P&L for a long/short position.
#[must_use]
pub fn unrealized_pnl(entry: Price, current: Price, qty: Quantity, side: PositionSide) -> Money {
    let diff = match side {
        PositionSide::Long => current.0 - entry.0,
        PositionSide::Short => entry.0 - current.0,
    };
    Money(diff * qty.0)
}

/// Net exposure in quote currency = price * qty (signed by side).
#[must_use]
pub fn exposure(price: Price, qty: Quantity, side: PositionSide) -> Money {
    let signed = match side {
        PositionSide::Long => qty.0,
        PositionSide::Short => -qty.0,
    };
    Money(price.0 * signed)
}
