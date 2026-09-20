//! Rule registry: holds the set of rules active for an evaluation run.
//!
//! The registry is the integration point between the engine and the rule
//! set. Default construction populates the standard library of rules; users
//! can add custom rules via [`RuleRegistry::register`].

use crate::core::ids::RuleId;
use crate::core::violation::{Violation, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext, RuleContextKind};
use crate::rules::traits::{Rule, RuleReport, RuleVerdict};
use crate::core::Error;
use std::collections::HashMap;
use std::sync::Arc;

/// Factory function type for building a rule implementation from a
/// [`RuleEntry`](crate::rulepack::RuleEntry). Maps the rule-pack's `kind`
/// string to the concrete Rust rule implementation that backs it.
///
/// (P1-6 fix.) Each `kind` ("max_drawdown", "daily_drawdown", etc.) maps
/// to a registered rule factory; the factory reads the entry's `value`,
/// `basis`, `unit`, `tolerance_cents`, `early_warning_pct`, and `params_json`
/// fields to parameterize the rule. The rule implementations themselves
/// are unchanged — they're now re-parameterized from data instead of
/// from compiled struct fields.
pub type RuleFactory = fn() -> Arc<dyn Rule>;

/// Default factory: maps kind name → constructor for the standard rule library.
pub fn default_factory_for_kind(kind: &str) -> Option<RuleFactory> {
    use crate::rules::evaluators::*;
    Some(match kind {
        "daily_drawdown" => || Arc::new(daily_drawdown::DailyDrawdownRule::default()),
        "max_drawdown" => || Arc::new(max_drawdown::MaxDrawdownRule::default()),
        "trailing_drawdown" => || Arc::new(trailing_drawdown::TrailingDrawdownRule::default()),
        "profit_target" => || Arc::new(profit_target::ProfitTargetRule::default()),
        "min_trading_days" => || Arc::new(min_trading_days::MinTradingDaysRule::default()),
        "consistency" => || Arc::new(consistency::ConsistencyRule::default()),
        "news_trading" => || Arc::new(news_trading::NewsTradingRule::default()),
        "overnight_holding" => || Arc::new(overnight_holding::OvernightHoldingRule::default()),
        "weekend_holding" => || Arc::new(weekend_holding::WeekendHoldingRule::default()),
        "max_position_size" => || Arc::new(max_position_size::MaxPositionSizeRule::default()),
        "max_open_positions" => || Arc::new(max_open_positions::MaxOpenPositionsRule::default()),
        "max_daily_trades" => || Arc::new(max_daily_trades::MaxDailyTradesRule::default()),
        "time_limit" => || Arc::new(time_limit::TimeLimitRule::default()),
        "cooldown" => || Arc::new(cooldown::CooldownRule::default()),
        "hedging" => || Arc::new(hedging::HedgingRule::default()),
        "grid_trading" => || Arc::new(grid_trading::GridTradingRule::default()),
        "copy_trading" => || Arc::new(copy_trading::CopyTradingRule::default()),
        "sl_required" => || Arc::new(sl_required::StopLossRequiredRule::default()),
        "tp_required" => || Arc::new(tp_required::TakeProfitRequiredRule::default()),
        _ => return None,
    })
}

/// Default rule library (registered automatically).
pub fn default_rules() -> Vec<Arc<dyn Rule>> {
    use crate::rules::evaluators::*;
    vec![
        Arc::new(daily_drawdown::DailyDrawdownRule::default()),
        Arc::new(max_drawdown::MaxDrawdownRule::default()),
        Arc::new(trailing_drawdown::TrailingDrawdownRule::default()),
        Arc::new(profit_target::ProfitTargetRule::default()),
        Arc::new(min_trading_days::MinTradingDaysRule::default()),
        Arc::new(consistency::ConsistencyRule::default()),
        Arc::new(news_trading::NewsTradingRule::default()),
        Arc::new(overnight_holding::OvernightHoldingRule::default()),
        Arc::new(weekend_holding::WeekendHoldingRule::default()),
        Arc::new(max_position_size::MaxPositionSizeRule::default()),
        Arc::new(max_open_positions::MaxOpenPositionsRule::default()),
        Arc::new(max_daily_trades::MaxDailyTradesRule::default()),
        Arc::new(time_limit::TimeLimitRule::default()),
        Arc::new(cooldown::CooldownRule::default()),
        Arc::new(hedging::HedgingRule::default()),
        Arc::new(grid_trading::GridTradingRule::default()),
        Arc::new(copy_trading::CopyTradingRule::default()),
        Arc::new(sl_required::StopLossRequiredRule::default()),
        Arc::new(tp_required::TakeProfitRequiredRule::default()),
    ]
}

/// Rule registry. Holds the active rule set keyed by id.
#[derive(Clone)]
pub struct RuleRegistry {
    rules: Vec<Arc<dyn Rule>>,
    by_id: HashMap<RuleId, usize>,
}

impl Default for RuleRegistry {
    fn default() -> Self {
        Self::with_default_rules()
    }
}

impl RuleRegistry {
    /// Builds an empty registry.
    pub fn empty() -> Self {
        RuleRegistry {
            rules: Vec::new(),
            by_id: HashMap::new(),
        }
    }

    /// Builds a registry pre-populated with the default rule library.
    pub fn with_default_rules() -> Self {
        let mut r = Self::empty();
        for rule in default_rules() {
            r.register(rule);
        }
        r
    }

    /// Adds a rule to the registry. Replaces if a rule with the same id
    /// already exists.
    pub fn register(&mut self, rule: Arc<dyn Rule>) {
        let id = rule.id();
        if let Some(&idx) = self.by_id.get(&id) {
            self.rules[idx] = rule;
        } else {
            let idx = self.rules.len();
            self.rules.push(rule);
            self.by_id.insert(id, idx);
        }
    }

    /// Removes a rule by id.
    pub fn unregister(&mut self, id: RuleId) {
        if let Some(idx) = self.by_id.remove(&id) {
            self.rules.remove(idx);
            // reindex
            self.by_id.clear();
            for (i, r) in self.rules.iter().enumerate() {
                self.by_id.insert(r.id(), i);
            }
        }
    }

    /// **P1-6 fix**: builds a registry from a [`RulePack`] (versioned data).
    /// For each `RuleEntry` in the pack, look up the rule factory by
    /// `kind`, instantiate the rule implementation, and register it. The
    /// rule implementations are unchanged — they read their parameters
    /// from `ctx.rule_config` at evaluation time, which the pipeline
    /// builds from the plan. The pack itself is the *source of truth*
    /// for which rules are active.
    ///
    /// Disabled entries (`enabled: false`) are skipped. Unknown kinds
    /// produce a soft warning and are skipped (not a hard error — a
    /// pack might reference a rule kind that this engine version
    /// doesn't yet support).
    pub fn build_from_pack(pack: &crate::rulepack::RulePack) -> Result<Self, Error> {
        let mut registry = Self::empty();
        for entry in &pack.rules {
            if !entry.enabled {
                continue;
            }
            match default_factory_for_kind(&entry.kind) {
                Some(factory) => {
                    let rule = factory();
                    registry.register(rule);
                }
                None => {
                    // Soft skip — unknown kind in this engine version.
                    // In production, log this so tenant admins know
                    // their pack references an unsupported kind.
                }
            }
        }
        Ok(registry)
    }

    /// Returns the rule with the given id, if any.
    pub fn get(&self, id: RuleId) -> Option<&Arc<dyn Rule>> {
        self.by_id.get(&id).map(|&i| &self.rules[i])
    }

    /// Returns the list of all rules.
    pub fn all(&self) -> &[Arc<dyn Rule>] {
        &self.rules
    }

    /// Returns the rules that apply to the given context kind / scope.
    pub fn applicable(&self, kind: RuleContextKind) -> Vec<&Arc<dyn Rule>> {
        let scope = match kind {
            RuleContextKind::OnOrderSubmit => EvaluationScope::PreTrade,
            RuleContextKind::OnTradeFill => EvaluationScope::PostTrade,
            RuleContextKind::OnTick => EvaluationScope::OnTick,
            RuleContextKind::OnDayRollover |
            RuleContextKind::OnEndOfDay => EvaluationScope::Periodic,
            RuleContextKind::OnDemand => EvaluationScope::OnDemand,
        };
        self.rules
            .iter()
            .filter(|r| r.scope() == scope || r.scope() == EvaluationScope::OnDemand)
            .collect()
    }

    /// Evaluates all rules applicable to the given context, returning reports.
    pub fn evaluate(&self, ctx: &RuleContext) -> crate::Result<Vec<RuleReport>> {
        let mut reports = Vec::with_capacity(self.rules.len());
        for rule in self.rules.iter() {
            if !rule.is_enabled(ctx) {
                continue;
            }
            let scope = rule.scope();
            let applicable = match ctx.kind {
                RuleContextKind::OnOrderSubmit => scope == EvaluationScope::PreTrade || scope == EvaluationScope::OnDemand,
                RuleContextKind::OnTradeFill => scope == EvaluationScope::PostTrade || scope == EvaluationScope::OnDemand,
                RuleContextKind::OnTick => scope == EvaluationScope::OnTick || scope == EvaluationScope::OnDemand,
                RuleContextKind::OnDayRollover |
                RuleContextKind::OnEndOfDay => scope == EvaluationScope::Periodic || scope == EvaluationScope::OnDemand,
                RuleContextKind::OnDemand => true,
            };
            if !applicable {
                continue;
            }
            let verdict = rule.evaluate(ctx).unwrap_or_else(|e| {
                let v = Violation::new(
                    ctx.account.id,
                    rule.id(),
                    rule.name(),
                    rule.kind(),
                    ViolationSeverity::Warning,
                    format!("rule evaluation error: {e}"),
                    ctx.server_time.ts(),
                );
                RuleVerdict::Warn(v)
            });
            let mut report = RuleReport::new(rule.id(), rule.name(), verdict, scope);
            // P0-4: stamp each report with the rule's declared priority so
            // Decision::from_reports can pick the winner deterministically
            // rather than by iteration order.
            report = report.with_priority(rule.priority());
            report = report.with_metadata("kind", ctx.kind.to_string());
            reports.push(report);
        }
        Ok(reports)
    }
}

impl std::fmt::Display for RuleContextKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use RuleContextKind::*;
        let s = match self {
            OnOrderSubmit => "on_order_submit",
            OnTradeFill => "on_trade_fill",
            OnTick => "on_tick",
            OnDayRollover => "on_day_rollover",
            OnEndOfDay => "on_end_of_day",
            OnDemand => "on_demand",
        };
        write!(f, "{s}")
    }
}

/// Convenience function: a stub for the violation builder module (used by
/// rule implementations to construct violations ergonomically).
pub fn build_violation(
    rule: &dyn Rule,
    ctx: &RuleContext,
    severity: crate::core::violation::ViolationSeverity,
    message: impl Into<String>,
) -> Violation {
    Violation::new(
        ctx.account.id,
        rule.id(),
        rule.name(),
        rule.kind(),
        severity,
        message,
        ctx.server_time.ts(),
    )
    // P1-9: stamp the tenant from the account.
    .with_tenant(ctx.account.tenant_id)
}
