//! Numeric and primitive types used throughout the engine.
//!
//! All monetary values use [`rust_decimal::Decimal`] (re-exported as
//! [`Decimal`]) to avoid IEEE-754 drift. Semantic newtype wrappers
//! ([`Money`], [`Price`], [`Quantity`], [`Lots`], [`Pct`]) make function
//! signatures self-documenting and prevent argument mix-ups.

use crate::core::Error;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use std::ops::{Add, AddAssign, Sub, SubAssign};
use std::str::FromStr;

/// Re-export of `rust_decimal::Decimal` so callers can write `propfirm::core::types::Decimal`.
pub use rust_decimal::Decimal;

/// Re-export for the `dec!` macro for ergonomic literals.
pub use rust_decimal_macros::dec;

/// Newtype for monetary values, e.g. balance, equity, pnl.
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Money(pub Decimal);

/// Newtype for prices (quotes, ticks).
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Price(pub Decimal);

/// Newtype for tradeable quantities (volume in units, not lots).
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Quantity(pub Decimal);

/// Newtype for lot sizes (1 lot = 100,000 units conventionally for FX majors).
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Lots(pub Decimal);

/// Newtype for percentages stored as a fraction (0.05 = 5%).
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Pct(pub Decimal);

/// Trading instrument symbol (e.g. "EURUSD").
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Symbol(pub String);

/// Account leverage (e.g. 1:100).
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Leverage(pub u32);

/// Server-side time abstraction.
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerTime(pub Timestamp);

/// UTC timestamp.
pub type Timestamp = DateTime<Utc>;

/// Calendar date (no time component).
pub type Date = NaiveDate;

/// Wall-clock time (no date component).
pub type Time = NaiveTime;

/// Server-side time abstraction (alias kept for backward compatibility).
pub type ServerTimeAlias = ServerTime;

/// Duration in milliseconds. Used for cooldowns and round-trip latency.
pub type Duration = chrono::Duration;

// ---------------------------------------------------------------------------
// Money
// ---------------------------------------------------------------------------

impl Money {
    pub const ZERO: Money = Money(Decimal::ZERO);

    #[must_use]
    pub fn new(v: Decimal) -> Self {
        Money(v)
    }
    /// Parses a decimal string into a `Money` value.
    ///
    /// Note: deliberately *not* the `std::str::FromStr` impl so the error
    /// type can be the crate's [`Error`](crate::core::Error) rather than a
    /// placeholder; use `s.parse::<MoneyFromStr>` via [`Decimal::from_str`]
    /// alternatives when the trait form is needed.
    pub fn parse_money(s: &str) -> crate::Result<Self> {
        Decimal::from_str(s)
            .map(Money)
            .map_err(|e| Error::NumericConversion(e.to_string()))
    }
    #[must_use]
    pub fn raw(self) -> Decimal {
        self.0
    }
    #[must_use]
    pub fn is_negative(self) -> bool {
        self.0.is_sign_negative()
    }
    #[must_use]
    pub fn abs(self) -> Self {
        Money(self.0.abs())
    }
    #[must_use]
    pub fn max(self, other: Self) -> Self {
        Money(self.0.max(other.0))
    }
    #[must_use]
    pub fn min(self, other: Self) -> Self {
        Money(self.0.min(other.0))
    }
}

impl std::fmt::Display for Money {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl Add for Money {
    type Output = Money;
    fn add(self, rhs: Self) -> Self {
        Money(self.0 + rhs.0)
    }
}
impl Sub for Money {
    type Output = Money;
    fn sub(self, rhs: Self) -> Self {
        Money(self.0 - rhs.0)
    }
}
impl AddAssign for Money {
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}
impl SubAssign for Money {
    fn sub_assign(&mut self, rhs: Self) {
        self.0 -= rhs.0;
    }
}

// ---------------------------------------------------------------------------
// Price
// ---------------------------------------------------------------------------

impl Price {
    pub const ZERO: Price = Price(Decimal::ZERO);
    #[must_use]
    pub fn new(v: Decimal) -> Self {
        Price(v)
    }
    #[must_use]
    pub fn raw(self) -> Decimal {
        self.0
    }
}

impl std::fmt::Display for Price {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// Quantity
// ---------------------------------------------------------------------------

impl Quantity {
    pub const ZERO: Quantity = Quantity(Decimal::ZERO);
    #[must_use]
    pub fn new(v: Decimal) -> Self {
        Quantity(v)
    }
    #[must_use]
    pub fn raw(self) -> Decimal {
        self.0
    }
    #[must_use]
    pub fn is_positive(self) -> bool {
        self.0.is_sign_positive() && !self.0.is_zero()
    }
}

impl std::fmt::Display for Quantity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// Lots
// ---------------------------------------------------------------------------

impl Lots {
    pub const ZERO: Lots = Lots(Decimal::ZERO);
    #[must_use]
    pub fn new(v: Decimal) -> Self {
        Lots(v)
    }
    #[must_use]
    pub fn raw(self) -> Decimal {
        self.0
    }
}

impl std::fmt::Display for Lots {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// Pct
// ---------------------------------------------------------------------------

impl Pct {
    pub const ZERO: Pct = Pct(Decimal::ZERO);
    #[must_use]
    pub fn new(v: Decimal) -> Self {
        Pct(v)
    }
    #[must_use]
    pub fn from_pct(v: f64) -> Self {
        Pct(Decimal::from_str(&v.to_string()).unwrap_or(Decimal::ZERO))
    }
    #[must_use]
    pub fn raw(self) -> Decimal {
        self.0
    }
    #[must_use]
    pub fn as_percent(self) -> Decimal {
        self.0 * dec!(100)
    }
    #[must_use]
    pub fn of(self, m: Money) -> Money {
        Money(self.0 * m.0)
    }
}

impl std::fmt::Display for Pct {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}%", self.0 * dec!(100))
    }
}

impl From<Decimal> for Pct {
    fn from(v: Decimal) -> Self {
        Pct(v)
    }
}

impl From<f64> for Pct {
    fn from(v: f64) -> Self {
        let d = Decimal::from_str(&v.to_string()).unwrap_or(Decimal::ZERO);
        Pct(d)
    }
}

// ---------------------------------------------------------------------------
// Symbol
// ---------------------------------------------------------------------------

impl Symbol {
    pub fn new(s: impl Into<String>) -> Self {
        Symbol(s.into().to_uppercase())
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Symbol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<&str> for Symbol {
    fn from(s: &str) -> Self {
        Symbol::new(s)
    }
}
impl From<String> for Symbol {
    fn from(s: String) -> Self {
        Symbol::new(s)
    }
}

// ---------------------------------------------------------------------------
// Leverage
// ---------------------------------------------------------------------------

impl Leverage {
    pub const ONE: Leverage = Leverage(1);
    #[must_use]
    pub fn new(v: u32) -> Self {
        Leverage(v)
    }
    #[must_use]
    pub fn raw(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for Leverage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "1:{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// ServerTime
// ---------------------------------------------------------------------------

impl ServerTime {
    #[must_use]
    pub fn now() -> Self {
        ServerTime(Utc::now())
    }
    #[must_use]
    pub fn at(ts: Timestamp) -> Self {
        ServerTime(ts)
    }
    #[must_use]
    pub fn ts(self) -> Timestamp {
        self.0
    }
}

impl std::fmt::Display for ServerTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.to_rfc3339())
    }
}
