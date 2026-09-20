//! Broker-is-truth equity input (P1-5 fix).
//!
//! The binding platform spec is explicit and non-negotiable: broker-reported
//! equity is **never recomputed**; the broker is the sole source of truth.
//! A shadow ledger that tracks fills, partial fills, slippage, and swap
//! timing independently *will* drift from the broker's actual books over
//! time, and when it does, you've failed or passed a real trader's account
//! based on a number that doesn't match their statement — which is the
//! single worst failure mode this whole system exists to avoid.
//!
//! This module makes the distinction explicit in the type system:
//!
//! - [`EquityInput::BrokerReported`] — the broker's own equity number, as
//!   reported by the bridge/BRG module. **The only equity value that can
//!   drive a `Fail`/`Liquidate` verdict.** If only an estimate is available,
//!   breach-capable rules must downgrade their verdict to `Warn` at most
//!   (or `Pass` if no warning threshold is crossed).
//!
//! - [`EquityInput::Estimated`] — a number derived from the engine's own
//!   position bookkeeping + a raw market quote (the previous behavior). Useful
//!   for display estimates between broker syncs and for backtesting, but
//!   must never be the number that decides a breach.
//!
//! This is enforced at the `RuleContext` level: a context built from an
//! `Estimated` equity carries a flag that breach-capable rules can check
//! via [`RuleContext::equity_is_broker_reported`].

use crate::core::types::Money;

/// Tagged equity input that distinguishes broker-reported from estimated
/// values. The tag is preserved end-to-end through evaluation so breach
/// rules can refuse to terminate on an estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EquityInput {
    /// Broker-reported equity — the broker's own number, as delivered by
    /// the bridge/BRG module. **The only equity value that can drive a
    /// `Fail`/`Liquidate` verdict.**
    BrokerReported {
        /// Reported equity (balance + unrealized P&L per the broker).
        equity: Money,
        /// Reported balance (cash, no floating P&L).
        balance: Money,
    },
    /// Engine-derived estimate (the previous behavior — balance + our own
    /// position marks against a raw quote). Display/backtest only; cannot
    /// drive a breach verdict.
    Estimated {
        /// Estimated equity.
        equity: Money,
        /// Estimated balance (typically the last broker-synced balance).
        balance: Money,
    },
}

impl EquityInput {
    /// Returns the equity value regardless of source. Use this only for
    /// display / non-breach purposes. For breach decisions, prefer
    /// [`Self::broker_equity`] which returns `None` if the value is an
    /// estimate.
    #[must_use]
    pub fn equity(self) -> Money {
        match self {
            EquityInput::BrokerReported { equity, .. } | EquityInput::Estimated { equity, .. } => {
                equity
            }
        }
    }

    /// Returns the balance value regardless of source.
    #[must_use]
    pub fn balance(self) -> Money {
        match self {
            EquityInput::BrokerReported { balance, .. }
            | EquityInput::Estimated { balance, .. } => balance,
        }
    }

    /// Returns `true` only if this is a broker-reported value. Breach-capable
    /// rules check this before emitting `Fail`/`Liquidate`.
    #[must_use]
    pub fn is_broker_reported(self) -> bool {
        matches!(self, EquityInput::BrokerReported { .. })
    }

    /// Returns the broker-reported equity if and only if this input is
    /// broker-reported; otherwise `None`. Use this in breach-capable rules
    /// to refuse termination on an estimate:
    ///
    /// ```rust,ignore
    /// let equity = match ctx.equity_input.broker_equity() {
    ///     Some(e) => e,
    ///     None => {
    ///         // Estimate only — downgrade to Warn.
    ///         return Ok(RuleVerdict::Warn(violation));
    ///     }
    /// };
    /// ```
    #[must_use]
    pub fn broker_equity(self) -> Option<Money> {
        match self {
            EquityInput::BrokerReported { equity, .. } => Some(equity),
            EquityInput::Estimated { .. } => None,
        }
    }
}

impl Default for EquityInput {
    fn default() -> Self {
        // Conservative default: assume estimate. Forces callers to
        // explicitly opt in to broker-reported by constructing it.
        EquityInput::Estimated {
            equity: Money::ZERO,
            balance: Money::ZERO,
        }
    }
}

impl std::fmt::Display for EquityInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EquityInput::BrokerReported { equity, balance } => {
                write!(f, "broker(equity={equity}, balance={balance})")
            }
            EquityInput::Estimated { equity, balance } => {
                write!(f, "estimated(equity={equity}, balance={balance})")
            }
        }
    }
}
