//! §C.2 rules for plan fields that existed but were never enforced:
//! `max_total_lots`, `trading_hours`, and leverage/margin.
//!
//! All three read the instrument registry from the rule context for
//! units↔lots conversion (§C.1); unregistered symbols use the
//! 1-unit-per-lot fallback.

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::ParameterizedRule;
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};
use chrono::Timelike;

// ---------------------------------------------------------------------------
// MaxTotalLotsRule — aggregate open quantity + pending order, in lots.
// ---------------------------------------------------------------------------

/// Caps the account's aggregate open exposure (open positions plus the
/// pending order) at `plan.max_total_lots`, converted through the
/// instrument spec.
#[derive(Debug, Clone, Default)]
pub struct MaxTotalLotsRule {
    /// Pack override; `value` is the max aggregate lots (count semantics).
    pub params: Option<crate::rules::params::RuleParams>,
}

impl MaxTotalLotsRule {
    fn effective_max_lots(
        &self,
        ctx: &RuleContext,
    ) -> Result<Option<rust_decimal::Decimal>, crate::core::Error> {
        if let Some(p) = &self.params {
            if !p.enabled {
                return Ok(None);
            }
            let Some(v) = p.value() else {
                return Ok(None);
            };
            return Ok(Some(v));
        }
        Ok(ctx.account.plan.max_total_lots)
    }
}

impl Rule for MaxTotalLotsRule {
    fn id(&self) -> RuleId {
        RuleId::named("max_total_lots")
    }
    fn name(&self) -> &'static str {
        "Max Total Lots"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::MaxLotSize
    }
    fn scope(&self) -> EvaluationScope {
        EvaluationScope::PreTrade
    }
    fn severity(&self) -> ViolationSeverity {
        ViolationSeverity::Hard
    }
    // P-failure-policy: expose pack-derived params for registry error mapping.
    fn params(&self) -> Option<&crate::rules::params::RuleParams> {
        self.params.as_ref()
    }

    fn description(&self) -> &'static str {
        "Forbids the aggregate open quantity (positions + pending order) exceeding max_total_lots."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        if let Some(p) = &self.params {
            return p.enabled && p.value.is_some();
        }
        ctx.account.plan.max_total_lots.is_some()
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        let Some(max_lots) = self
            .effective_max_lots(ctx)
            .map_err(|e| crate::core::Error::RuleEval(format!("max_total_lots: {e}")))?
        else {
            return Ok(RuleVerdict::Pass);
        };
        // Aggregate: per-position lots (via each symbol's spec) for open
        // positions across ALL symbols plus the pending order.
        let mut total_lots = crate::core::types::Lots::ZERO;
        for p in ctx.open_positions.iter().filter(|p| p.is_open()) {
            let spec = ctx.instruments.get(&p.symbol);
            total_lots += spec.units_to_lots(p.open_quantity);
        }
        if let Some(o) = &ctx.pending_order {
            let spec = ctx.instruments.get(&o.symbol);
            total_lots += spec.units_to_lots(o.quantity);
        }
        if total_lots.0 > max_lots {
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Hard,
                format!(
                    "Total exposure {} lots exceeds account limit {} lots",
                    total_lots.0, max_lots
                ),
            );
            return Ok(RuleVerdict::Fail(v));
        }
        Ok(RuleVerdict::Pass)
    }
}

impl MaxTotalLotsRule {
    /// Constructs a parameterized rule from a pack entry.
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        MaxTotalLotsRule {
            params: Some(crate::rules::params::RuleParams::from_entry(entry)),
        }
    }
}

impl ParameterizedRule for MaxTotalLotsRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        MaxTotalLotsRule {
            params: Some(crate::rules::params::RuleParams::from_entry(entry)),
        }
    }
}

// ---------------------------------------------------------------------------
// TradingHoursRule — reject orders outside plan.trading_hours (plan tz).
// ---------------------------------------------------------------------------

/// Rejects orders submitted outside `plan.trading_hours` (a
/// `(start_hour, end_hour)` window evaluated in the plan's timezone).
#[derive(Debug, Clone, Default)]
pub struct TradingHoursRule;

impl Rule for TradingHoursRule {
    fn id(&self) -> RuleId {
        RuleId::named("trading_hours")
    }
    fn name(&self) -> &'static str {
        "Trading Hours"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::Custom
    }
    fn scope(&self) -> EvaluationScope {
        EvaluationScope::PreTrade
    }
    fn severity(&self) -> ViolationSeverity {
        ViolationSeverity::Hard
    }

    fn description(&self) -> &'static str {
        "Rejects orders submitted outside the plan's allowed trading hours (plan timezone)."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        ctx.account.plan.trading_hours.is_some()
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        let Some((start_hour, end_hour)) = ctx.account.plan.trading_hours else {
            return Ok(RuleVerdict::Pass);
        };
        let Some(order) = &ctx.pending_order else {
            return Ok(RuleVerdict::Pass);
        };
        // Local hour in the plan's timezone (UTC when None).
        let local_hour: u32 = match ctx.account.plan.timezone {
            Some(tz) => order.submitted_at.with_timezone(&tz).hour(),
            None => order.submitted_at.hour(),
        };
        // Window wraps midnight (e.g. 22..=6): allowed if hour >= start || hour < end.
        let in_window = if start_hour <= end_hour {
            local_hour >= u32::from(start_hour) && local_hour < u32::from(end_hour)
        } else {
            local_hour >= u32::from(start_hour) || local_hour < u32::from(end_hour)
        };
        if !in_window {
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Hard,
                format!(
                    "Order submitted at local hour {local_hour}, outside allowed window {start_hour:02}:00-{end_hour:02}:00"
                ),
            );
            return Ok(RuleVerdict::Fail(v));
        }
        Ok(RuleVerdict::Pass)
    }
}

impl TradingHoursRule {
    /// Constructs a rule from a pack entry (no pack-settable fields).
    #[must_use]
    pub fn from_entry(_entry: &crate::rulepack::RuleEntry) -> Self {
        TradingHoursRule
    }
}

impl ParameterizedRule for TradingHoursRule {
    fn from_entry(_entry: &crate::rulepack::RuleEntry) -> Self {
        TradingHoursRule
    }
}

// ---------------------------------------------------------------------------
// MarginRule — prospective margin at plan.leverage must be covered.
// ---------------------------------------------------------------------------

/// Requires margin for the prospective position (order units × price ÷
/// plan leverage) to fit within free margin: equity minus the notional
/// already committed by open positions. Fails the order when margin is
/// insufficient.
#[derive(Debug, Clone, Default)]
pub struct MarginRule;

impl MarginRule {
    /// Free margin = equity − margin already used by open positions.
    fn free_margin(&self, ctx: &RuleContext) -> rust_decimal::Decimal {
        let equity = ctx.account.equity.0;
        let mut used = rust_decimal::Decimal::ZERO;
        for p in ctx.open_positions.iter().filter(|p| p.is_open()) {
            let spec = ctx.instruments.get(&p.symbol);
            let notional = spec.notional(spec.units_to_lots(p.open_quantity), p.avg_entry_price);
            used += notional.0 / rust_decimal::Decimal::from(ctx.account.plan.leverage);
        }
        equity - used
    }
}

impl Rule for MarginRule {
    fn id(&self) -> RuleId {
        RuleId::named("margin")
    }
    fn name(&self) -> &'static str {
        "Margin"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::Custom
    }
    fn scope(&self) -> EvaluationScope {
        EvaluationScope::PreTrade
    }
    fn severity(&self) -> ViolationSeverity {
        ViolationSeverity::Hard
    }

    fn description(&self) -> &'static str {
        "Rejects orders whose prospective margin (units × price ÷ leverage) exceeds free margin."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        // Always on: leverage defaults to a sane 1:100 and margin math is
        // meaningful for any account. There is no plan flag that can
        // reasonably disable insolvency checks.
        let _ = ctx;
        true
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        let Some(order) = &ctx.pending_order else {
            return Ok(RuleVerdict::Pass);
        };
        if order.quantity.0 <= rust_decimal::Decimal::ZERO {
            return Ok(RuleVerdict::Pass);
        }
        let Some(price) = order.avg_fill_price.or(match &order.order_type {
            crate::core::order::OrderType::Limit { price } => Some(*price),
            _ => None,
        }) else {
            // Market order with no reference price: cannot size margin —
            // pass (the fill-time re-check is the bridge's job).
            return Ok(RuleVerdict::Pass);
        };
        let spec = ctx.instruments.get(&order.symbol);
        let notional = spec.notional(spec.units_to_lots(order.quantity), price).0;
        let required_margin = notional / rust_decimal::Decimal::from(ctx.account.plan.leverage);
        let free = self.free_margin(ctx);
        if required_margin > free {
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Hard,
                format!(
                    "Insufficient margin: order requires {required_margin} (notional {notional} at 1:{leverage}), free margin {free}",
                    leverage = ctx.account.plan.leverage
                ),
            );
            return Ok(RuleVerdict::Fail(v));
        }
        Ok(RuleVerdict::Pass)
    }
}

impl MarginRule {
    /// Constructs a rule from a pack entry (no pack-settable fields).
    #[must_use]
    pub fn from_entry(_entry: &crate::rulepack::RuleEntry) -> Self {
        MarginRule
    }
}

impl ParameterizedRule for MarginRule {
    fn from_entry(_entry: &crate::rulepack::RuleEntry) -> Self {
        MarginRule
    }
}
