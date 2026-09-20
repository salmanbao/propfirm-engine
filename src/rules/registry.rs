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
}
