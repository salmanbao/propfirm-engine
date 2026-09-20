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

pub mod daily_drawdown;
pub mod max_drawdown;
pub mod trailing_drawdown;
pub mod profit_target;
pub mod min_trading_days;
pub mod consistency;
pub mod news_trading;
pub mod overnight_holding;
pub mod weekend_holding;
pub mod max_position_size;
pub mod max_open_positions;
pub mod max_daily_trades;
pub mod time_limit;
pub mod cooldown;
pub mod hedging;
pub mod grid_trading;
pub mod copy_trading;
pub mod sl_required;
pub mod tp_required;
pub mod hft_scalping;          // P2.15 fix.
pub mod per_trade_max_loss;    // P2.15 fix.
