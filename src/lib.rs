//! # Prop Firm Risk & Rule Evaluation Engine
//!
//! An enterprise-grade engine for evaluating proprietary trading firm rules,
//! challenge progress, and account risk in real time.
//!
//! ## Overview
//!
//! The engine is organized as a layered library:
//!
//! 1. **Core domain** ([`core`]) – immutable value types and aggregates such
//!    as [`Account`], [`Position`], [`Order`], [`Trade`], [`Tick`], and
//!    [`Violation`]. All numeric values use fixed-point arithmetic via
//!    `rust_decimal::Decimal` to avoid floating point drift on money.
//!
//! 2. **Configuration** ([`config`]) – [`ChallengePlan`] definitions that
//!    describe the rules and targets of a funded-trader program (Phase 1 /
//!    Phase 2 / Funded), plus preset factories for popular prop firm styles
//!    (FTMO-style, MyForexFunds-style, The Funded Trader-style, etc.).
//!
//! 3. **Rules** ([`rules`]) – the [`Rule`] trait, [`RuleContext`], the
//!    [`RuleRegistry`], and a comprehensive library of rule evaluators
//!    covering daily/max/trailing drawdown, profit target, minimum trading
//!    days, consistency, news/overnight/weekend restrictions, position size
//!    limits, hedging, grid trading, copy trading detection, mandatory SL/TP,
//!    time limits, daily trade caps, and cooldowns.
//!
//! 4. **Engine** ([`engine`]) – the [`Evaluator`] that orchestrates rule
//!    evaluation, the [`Pipeline`] that materializes events into decisions,
//!    [`Snapshot`] for point-in-time account state, and [`Decision`] for
//!    accept/reject/terminate outcomes.
//!
//! 5. **Risk** ([`risk`]) – quantitative risk metrics: Sharpe ratio, Sortino
//!    ratio, Calmar ratio, profit factor, max drawdown, equity curve
//!    analytics, exposure, and parametric Value-at-Risk.
//!
//! 6. **Persistence** ([`persistence`]) – a storage trait with an in-memory
//!    implementation and extension points for SQL / KV stores.
//!
//! 7. **Events** ([`events`]) – an append-only audit log / event sourcing
//!    primitives for full replay of account history.
//!
//! 8. **Notifications** ([`notifications`]) – pluggable notifier trait for
//!    webhook / email / push delivery on rule violations.
//!
//! 9. **Reporting** ([`reporting`]) – builds structured [`PerformanceReport`]
//!    summaries combining rule status and risk metrics.
//!
//! 10. **API** ([`api`]) – an optional `axum`-based HTTP server exposing
//!     REST endpoints for account evaluation.
//!
//! ## Example
//!
//! ```no_run
//! use propfirm::prelude::*;
//! use propfirm::config::presets::ftmo_phase1;
//! use propfirm::core::account::Account;
//! use propfirm::core::ids::AccountId;
//! use propfirm::engine::evaluator::Evaluator;
//! use propfirm::rules::context::RuleContext;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let plan = ftmo_phase1();
//! let account = Account::new(AccountId::new(), plan.clone());
//! let evaluator = Evaluator::new(&plan);
//! let ctx = RuleContext::new(account);
//! let result = evaluator.evaluate(&ctx)?;
//! println!("decision: {:?}", result.decision.kind);
//! # Ok(()) }
//! ```
//!
//! [`Rule`]: rules::Rule
//! [`RuleContext`]: rules::RuleContext
//! [`RuleRegistry`]: rules::RuleRegistry
//! [`Evaluator`]: engine::Evaluator
//! [`Pipeline`]: engine::Pipeline
//! [`Snapshot`]: engine::Snapshot
//! [`Decision`]: engine::Decision
//! [`PerformanceReport`]: reporting::PerformanceReport

// `missing_docs` is deliberately NOT gated in CI yet: the crate predates a
// doc-coverage pass (~445 public items lack rustdoc). Re-enable once the
// docs are filled in; the clippy -D warnings gate covers everything else.
#![allow(missing_docs)]
#![warn(clippy::all)]
// The remaining opt-in style lints that pedantic would enable but that this
// crate intentionally does not enforce (kept off so `clippy -D warnings` is a
// useful CI gate): module_name_repetitions, missing_errors_doc, must_use
// (already covered by the standard attribute), unreadable_literal,
// cast-precision-loss, doc-markdown, struct_field_names.
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::missing_errors_doc)]

pub mod config;
pub mod copy_trading;
pub mod core;
pub mod engine;
pub mod equity_input;
pub mod events;
pub mod liquidation;
pub mod news_calendar;
pub mod notifications;
pub mod override_engine;
pub mod persistence;
pub mod pure;
pub mod reporting;
pub mod risk;
pub mod rulepack;
pub mod rules;
pub mod sha256_helper;
pub mod tenant;

#[cfg(feature = "server")]
pub mod api;

pub mod prelude;

/// Crate-level error type. Re-exported from [`core::Error`].
pub type Error = core::Error;

/// Crate-level result type.
pub type Result<T> = std::result::Result<T, Error>;

/// Semantic version string of the engine.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
