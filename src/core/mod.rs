//! Core domain types for the prop firm engine.
//!
//! This module holds the immutable value types and aggregates that describe
//! the trading domain. Money is represented as fixed-point `Decimal` to
//! eliminate floating-point drift on monetary calculations.

pub mod account;
pub mod events;
pub mod ids;
pub mod order;
pub mod position;
pub mod tick;
pub mod trade;
pub mod types;
pub mod violation;

use thiserror::Error;

/// Crate-level error type. Most public APIs that can fail return [`Result<T,
/// Error>`].
#[derive(Debug, Error)]
pub enum Error {
    /// A configuration value was invalid (e.g. negative drawdown limit).
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    /// A rule evaluation was attempted on an unsupported context kind.
    #[error("rule {0} not applicable to context kind {1}")]
    RuleNotApplicable(String, String),

    /// A numeric conversion could not be performed safely.
    #[error("numeric conversion error: {0}")]
    NumericConversion(String),

    /// A requested entity could not be found in storage.
    #[error("entity not found: {0}")]
    NotFound(String),

    /// A persistence operation failed.
    #[error("persistence error: {0}")]
    Persistence(String),

    /// A serialization error occurred.
    #[error("serialization error: {0}")]
    Serialization(String),

    /// A logical precondition was violated.
    #[error("invalid state: {0}")]
    InvalidState(String),

    /// A user-supplied rule produced an error.
    #[error("rule evaluation error: {0}")]
    RuleEval(String),
}

/// Convenience constructor for [`Error::InvalidConfig`].
pub fn invalid_config(msg: impl Into<String>) -> Error {
    Error::InvalidConfig(msg.into())
}

/// Convenience constructor for [`Error::InvalidState`].
pub fn invalid_state(msg: impl Into<String>) -> Error {
    Error::InvalidState(msg.into())
}

impl Error {
    /// Convenience constructor for [`Error::InvalidConfig`].
    pub fn invalid_config(msg: impl Into<String>) -> Error {
        Error::InvalidConfig(msg.into())
    }
    /// Convenience constructor for [`Error::InvalidState`].
    pub fn invalid_state(msg: impl Into<String>) -> Error {
        Error::InvalidState(msg.into())
    }
    /// Convenience constructor for [`Error::NotFound`].
    pub fn not_found(msg: impl Into<String>) -> Error {
        Error::NotFound(msg.into())
    }
}
