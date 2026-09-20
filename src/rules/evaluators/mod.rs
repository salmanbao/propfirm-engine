//! Concrete rule evaluators.
//!
//! Each file implements one rule. All rules follow the same pattern:
//!
//! 1. Implement [`Rule`](crate::rules::traits::Rule) for a struct with
//!    sensible `Default` values.
//! 2. In `evaluate`, check the context for the relevant state (balance,
//!    equity, open positions, recent trades, etc.).
//! 3. Return a [`RuleVerdict::Pass`], [`RuleVerdict::Warn`], [`RuleVerdict::Fail`],
//!    or [`RuleVerdict::Liquidate`] with a [`Violation`] when applicable.

pub mod consistency;
pub mod cooldown;
pub mod copy_trading;
pub mod daily_drawdown;
pub mod grid_trading;
pub mod hedging;
pub mod hft_scalping; // P2.15 fix.
pub mod max_daily_trades;
pub mod max_drawdown;
pub mod max_open_positions;
pub mod max_position_size;
pub mod min_trading_days;
pub mod news_trading;
pub mod overnight_holding;
pub mod per_trade_max_loss;
pub mod profit_target;
pub mod sl_required;
pub mod time_limit;
pub mod tp_required;
pub mod trailing_drawdown;
pub mod weekend_holding; // P2.15 fix.
