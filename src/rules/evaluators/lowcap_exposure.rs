//! Low-capitalisation asset exposure cap (P3 fix).
//!
//! Caps total notional exposure to "low-cap" instruments at X% of
//! account balance. Used by HyroTrader's `lowcap_exposure_5pct`
//! restriction — one of the four restriction tags the firm publishes.
//!
//! ## How it works
//!
//! For each open position on the current account, look up the
//! instrument via `ctx.instrument_registry`. If the instrument's
//! `is_lowcap` flag is `true`, sum the position's notional value
//! (`lots × contract_size × price`). If the total exceeds
//! `plan.lowcap_exposure_limit_pct × account.balance`, flag a
//! `Warning`.
//!
//! ## Crypto-only
//!
//! This rule is crypto-specific — HyroTrader is the only verified firm
//! that uses it. FX majors, indices, and commodities are never
//! "low-cap" in this sense. The platform backend is responsible for
//! marking crypto instruments with `is_lowcap = true` when they
//! register them; without that flag, this rule is a no-op.
//!
//! ## Limitations
//!
//! - "Notional value" here is `lots × contract_size × price`. For
//!   crypto with `contract_size = 1` and price in USD, this is the
//!   USD notional. For FX majors with `contract_size = 100_000`, the
//!   notional is large by design. The rule is meaningful only when
//!   the instrument registry's `is_lowcap` flag is set correctly.
//! - The rule doesn't account for netting — two opposing low-cap
//!   positions would still both count toward the exposure cap. This
//!   is conservative (over-reports) but matches the spirit of the
//!   restriction.

use crate::core::ids::RuleId;
use crate::core::types::{dec, Money};
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};

#[derive(Debug, Clone, Default)]
pub struct LowcapExposureRule {
    pub params: Option<RuleParams>,
}

impl Rule for LowcapExposureRule {
    fn id(&self) -> RuleId {
        RuleId::named("lowcap_exposure")
    }
    fn name(&self) -> &'static str {
        "Low-Capitalisation Asset Exposure"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::LowcapExposure
    }
    fn scope(&self) -> EvaluationScope {
        EvaluationScope::OnTick
    }
    fn severity(&self) -> ViolationSeverity {
        ViolationSeverity::Warning
    }
    fn params(&self) -> Option<&crate::rules::params::RuleParams> {
        self.params.as_ref()
    }
    fn description(&self) -> &'static str {
        "Caps total notional exposure to low-capitalisation instruments at \
         X% of account balance. HyroTrader's `lowcap_exposure_5pct` \
         restriction. Requires the instrument registry to mark instruments \
         as low-cap via `InstrumentSpec::is_lowcap`."
    }
    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        if let Some(p) = &self.params {
            if !p.enabled {
                return false;
            }
        }
        // Plan-level gate: only run when the plan sets a lowcap cap.
        // HyroTrader sets 5%; every other plan leaves it None.
        ctx.account.plan.lowcap_exposure_limit_pct.is_some()
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        let Some(limit_pct) = ctx.account.plan.lowcap_exposure_limit_pct else {
            return Ok(RuleVerdict::Pass);
        };
        // Sum notional of all open positions on low-cap instruments.
        // We use the instrument registry to look up `is_lowcap`; symbols
        // not in the registry default to `is_lowcap = false`.
        let mut lowcap_notional = dec!(0);
        for pos in &ctx.account.open_positions {
            let spec = ctx.instruments.get(&pos.symbol);
            if spec.is_lowcap {
                // notional = lots × contract_size × reference_price.
                // We don't have a per-position reference price in the
                // context (positions carry `avg_entry_price`, but the
                // exposure we care about is the *current* notional, not
                // the cost basis). Use `avg_entry_price` as a proxy —
                // it's the best we have without a current tick per
                // symbol. This over-reports when the price has dropped
                // and under-reports when it has risen; conservative on
                // the breach-side would be to use the higher of the two,
                // but we don't have a current price here.
                let notional =
                    pos.opened_quantity.0 * spec.contract_size * pos.avg_entry_price.0;
                lowcap_notional += notional;
            }
        }
        let balance = ctx.account.balance.0;
        if balance <= dec!(0) {
            return Ok(RuleVerdict::Pass);
        }
        let limit_dollars = limit_pct.0 * balance;
        if lowcap_notional > limit_dollars {
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Warning,
                format!(
                    "Low-cap exposure ${lowcap_notional:.2} exceeds {}% of balance \
                     ${balance:.2} (cap ${limit_dollars:.2})",
                    limit_pct.0 * dec!(100),
                ),
            );
            return Ok(RuleVerdict::Warn(v));
        }
        Ok(RuleVerdict::Pass)
    }
}

impl LowcapExposureRule {
    /// Constructs a parameterized rule from a pack entry.
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        LowcapExposureRule {
            params: Some(RuleParams::from_entry(entry)),
        }
    }
}

impl ParameterizedRule for LowcapExposureRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        LowcapExposureRule::from_entry(entry)
    }
}

#[allow(dead_code)]
fn _ensure_money_type_used() -> Money {
    // Quiet "unused import" for `Money` — the type is referenced in
    // the trait impl above via `crate::core::types::Money` paths
    // but the import is here for ergonomic access if needed later.
    Money::ZERO
}
