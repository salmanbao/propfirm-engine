//! Tick / quote model.
//!
//! A [`Tick`] is a single market-data update for a symbol. A [`Quote`] is the
//! bid/ask pair used for valuation and order fills.

use crate::core::types::{Price, Symbol, Timestamp};

/// Bid/ask quote for a single symbol at a point in time.
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quote {
    /// Bid price (where the market buys from you).
    pub bid: Price,
    /// Ask price (where the market sells to you).
    pub ask: Price,
    /// Server timestamp.
    pub ts: Timestamp,
}

impl Quote {
    /// Mid-market price.
    #[must_use]
    pub fn mid(self) -> Price {
        Price((self.bid.0 + self.ask.0) / rust_decimal::Decimal::TWO)
    }

    /// Spread (ask - bid). Always non-negative.
    #[must_use]
    pub fn spread(self) -> Price {
        Price(self.ask.0 - self.bid.0)
    }
}

/// A market-data tick carrying symbol, quote, and optional last-traded price.
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[derive(Debug, Clone)]
pub struct Tick {
    pub symbol: Symbol,
    pub quote: Quote,
    pub last: Option<Price>,
    pub bid_volume: Option<rust_decimal::Decimal>,
    pub ask_volume: Option<rust_decimal::Decimal>,
}

impl Tick {
    #[must_use]
    pub fn new(symbol: Symbol, quote: Quote) -> Self {
        Tick {
            symbol,
            quote,
            last: None,
            bid_volume: None,
            ask_volume: None,
        }
    }
}
