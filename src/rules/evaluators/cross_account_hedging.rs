//! Cross-account hedging rule (P3 fix).
//!
//! Detects when an open position on the *current* account is hedged by
//! an opposing position on a *sibling* (other) funded account under the
//! same tenant. Used by HyroTrader's `no_cross_account_hedging`
//! restriction — one of the four restriction tags the firm publishes.
//!
//! ## How it works
//!
//! The engine's evaluate contract (see `pure.rs::EvaluateInputs`) carries
//! a `cross_account_reference_trades` seam — a list of fills executed on
//! *other* accounts owned by the same tenant. This rule consumes that
//! seam: for each open position on the current account, it checks whether
//! any reference trade on the same symbol has the opposite side. If yes,
//! that's a cross-account hedge and the rule flags a `Warning` (the
//! trader is notified but the account continues — the platform backend
//! is the actual enforcer; this rule is an early-warning signal).
//!
//! ## Limitations
//!
//! - The reference data is *trades* (fills), not *current open positions*
//!   on sibling accounts. A reference trade might have already been
//!   closed by the time this rule runs. The rule will over-report in
//!   that case (flag a hedge that no longer exists). The platform
//!   backend's authoritative enforcement should reject the trade at
//!   execution time, not rely on this rule's verdict.
//! - The rule can only detect hedging on the same symbol. Cross-symbol
//!   hedges (e.g. long EURUSD on one account, short GBPUSD on another)
//!   are not detectable from trade data alone.
//!
//! Despite those limitations, the rule is useful as a real-time
//! monitoring signal — operators can subscribe to the `cross_account_hedging`
//! violation kind and investigate when it fires.

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct CrossAccountHedgingRule {
    /// Pack-derived parameters (P0-D pattern; mirrors the other rules).
    /// The rule's `enabled` flag is honored, but the primary gate is
    /// `ChallengePlan::cross_account_hedging_prohibited`.
    pub params: Option<RuleParams>,
}

impl Rule for CrossAccountHedgingRule {
    fn id(&self) -> RuleId {
        RuleId::named("cross_account_hedging")
    }
    fn name(&self) -> &'static str {
        "Cross-Account Hedging"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::CrossAccountHedging
    }
    fn scope(&self) -> EvaluationScope {
        // Run on every tick — cross-account hedges can form/resolve at
        // any time as the platform backend feeds new reference trades.
        EvaluationScope::OnTick
    }
    fn severity(&self) -> ViolationSeverity {
        // Warning, not Hard/Liquidate — the platform backend is the
        // authoritative enforcer; this rule is an early-warning signal.
        ViolationSeverity::Warning
    }
    fn params(&self) -> Option<&crate::rules::params::RuleParams> {
        self.params.as_ref()
    }
    fn description(&self) -> &'static str {
        "Detects when an open position on the current account is hedged by \
         an opposing position on a sibling account under the same tenant. \
         HyroTrader's `no_cross_account_hedging` restriction."
    }
    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        if let Some(p) = &self.params {
            if !p.enabled {
                return false;
            }
        }
        // Plan-level gate: only run when the plan prohibits cross-account
        // hedging. HyroTrader sets this; every other plan leaves it off.
        ctx.account.plan.cross_account_hedging_prohibited
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        // For each open position on the current account, check if any
        // cross-account reference trade on the same symbol has the
        // opposite side. If yes, flag.
        //
        // `pos.side` is `PositionSide` (Long/Short);
        // `ref_trade.side` is `OrderSide` (Buy/Sell).
        // Compare via `PositionSide::from_order` to normalize.
        use crate::core::position::PositionSide;
        let mut hedged_symbols: Vec<String> = Vec::new();
        for pos in &ctx.account.open_positions {
            for ref_trade in &ctx.cross_reference_trades {
                if pos.symbol == ref_trade.symbol
                    && pos.side != PositionSide::from_order(ref_trade.side)
                {
                    hedged_symbols.push(pos.symbol.0.clone());
                    break;
                }
            }
        }
        if hedged_symbols.is_empty() {
            return Ok(RuleVerdict::Pass);
        }
        let v = build_violation(
            self,
            ctx,
            ViolationSeverity::Warning,
            format!(
                "Cross-account hedging detected on {} symbol(s): {} \
                 (open position on this account hedged by opposing fill on a \
                 sibling account under the same tenant)",
                hedged_symbols.len(),
                hedged_symbols.join(", ")
            ),
        );
        Ok(RuleVerdict::Warn(v))
    }
}

impl CrossAccountHedgingRule {
    /// Constructs a parameterized rule from a pack entry.
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        CrossAccountHedgingRule {
            params: Some(RuleParams::from_entry(entry)),
        }
    }
}

impl ParameterizedRule for CrossAccountHedgingRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        CrossAccountHedgingRule::from_entry(entry)
    }
}
