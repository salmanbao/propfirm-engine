//! Rule registry: holds the set of rules active for an evaluation run.
//!
//! The registry is the integration point between the engine and the rule
//! set. Default construction populates the standard library of rules; users
//! can add custom rules via [`RuleRegistry::register`].

use crate::core::ids::RuleId;
use crate::core::violation::{Violation, ViolationSeverity};
use crate::core::Error;
use crate::rules::context::{EvaluationScope, RuleContext, RuleContextKind};
use crate::rules::traits::{Rule, RuleReport};
use std::collections::HashMap;
use std::sync::Arc;

/// Factory function type for building a rule implementation from a
/// [`RuleEntry`](crate::rulepack::RuleEntry). Maps the rule-pack's `kind`
/// string to the concrete Rust rule implementation that backs it.
///
/// (P1-6 fix, P0-D fix.) Each `kind` ("`max_drawdown`", "`daily_drawdown`", etc.) maps
/// to a registered rule factory; the factory reads the entry's `value`,
/// `basis`, `unit`, `tolerance_cents`, `early_warning_pct`, `priority`,
/// and `params_json` fields to construct a parameterized rule. The rule
/// then reads from its own `params` field in `evaluate` instead of from
/// `ctx.account.plan` — so a tenant editing the pack through the form
/// actually changes the verdict.
pub type RuleFactory = fn(&crate::rulepack::RuleEntry) -> Result<Arc<dyn Rule>, Error>;

/// Default factory: maps kind name → constructor for the standard rule library.
/// Each factory reads the `RuleEntry`'s `value`, `basis`, `unit`, etc. and
/// produces a parameterized rule.
#[must_use]
pub fn default_factory_for_kind(kind: &str) -> Option<RuleFactory> {
    use crate::rulepack::RuleEntry;
    use crate::rules::evaluators::{
        consistency, cooldown, copy_trading, daily_drawdown, grid_trading, hedging, hft_scalping,
        max_daily_trades, max_drawdown, max_open_positions, max_position_size, min_trading_days,
        news_trading, overnight_holding, per_trade_max_loss, profit_target, sl_required,
        time_limit, tp_required, trailing_drawdown, weekend_holding,
    };
    Some(match kind {
        "daily_drawdown" => {
            |e: &RuleEntry| Ok(Arc::new(daily_drawdown::DailyDrawdownRule::from_entry(e)))
        }
        "max_drawdown" => {
            |e: &RuleEntry| Ok(Arc::new(max_drawdown::MaxDrawdownRule::from_entry(e)))
        }
        "trailing_drawdown" => |e: &RuleEntry| {
            Ok(Arc::new(
                trailing_drawdown::TrailingDrawdownRule::from_entry(e),
            ))
        },
        "profit_target" => {
            |e: &RuleEntry| Ok(Arc::new(profit_target::ProfitTargetRule::from_entry(e)))
        }
        "min_trading_days" => |e: &RuleEntry| {
            Ok(Arc::new(min_trading_days::MinTradingDaysRule::from_entry(
                e,
            )))
        },
        "consistency" => |e: &RuleEntry| Ok(Arc::new(consistency::ConsistencyRule::from_entry(e))),
        "news_trading" => {
            |e: &RuleEntry| Ok(Arc::new(news_trading::NewsTradingRule::from_entry(e)))
        }
        "overnight_holding" => |e: &RuleEntry| {
            Ok(Arc::new(
                overnight_holding::OvernightHoldingRule::from_entry(e),
            ))
        },
        "weekend_holding" => {
            |e: &RuleEntry| Ok(Arc::new(weekend_holding::WeekendHoldingRule::from_entry(e)))
        }
        "max_position_size" => |e: &RuleEntry| {
            Ok(Arc::new(
                max_position_size::MaxPositionSizeRule::from_entry(e),
            ))
        },
        "max_open_positions" => |e: &RuleEntry| {
            Ok(Arc::new(
                max_open_positions::MaxOpenPositionsRule::from_entry(e),
            ))
        },
        "max_daily_trades" => |e: &RuleEntry| {
            Ok(Arc::new(max_daily_trades::MaxDailyTradesRule::from_entry(
                e,
            )))
        },
        "time_limit" => |e: &RuleEntry| Ok(Arc::new(time_limit::TimeLimitRule::from_entry(e))),
        "cooldown" => |e: &RuleEntry| Ok(Arc::new(cooldown::CooldownRule::from_entry(e))),
        "hedging" => |e: &RuleEntry| Ok(Arc::new(hedging::HedgingRule::from_entry(e))),
        "grid_trading" => {
            |e: &RuleEntry| Ok(Arc::new(grid_trading::GridTradingRule::from_entry(e)))
        }
        "copy_trading" => {
            |e: &RuleEntry| Ok(Arc::new(copy_trading::CopyTradingRule::from_entry(e)))
        }
        "sl_required" => {
            |e: &RuleEntry| Ok(Arc::new(sl_required::StopLossRequiredRule::from_entry(e)))
        }
        "tp_required" => {
            |e: &RuleEntry| Ok(Arc::new(tp_required::TakeProfitRequiredRule::from_entry(e)))
        }
        "hft_scalping" => {
            |e: &RuleEntry| Ok(Arc::new(hft_scalping::HftScalpingRule::from_entry(e)))
        }
        "per_trade_max_loss" => |e: &RuleEntry| {
            Ok(Arc::new(
                per_trade_max_loss::PerTradeMaxLossRule::from_entry(e),
            ))
        },
        "inactivity" => |e: &RuleEntry| {
            Ok(Arc::new(
                crate::rules::evaluators::inactivity::InactivityRule::from_entry(e),
            ))
        },
        // §C.2: rules for the previously-unenforced plan fields.
        "max_total_lots" => |e: &RuleEntry| {
            Ok(Arc::new(
                crate::rules::evaluators::plan_caps::MaxTotalLotsRule::from_entry(e),
            ))
        },
        "trading_hours" => |e: &RuleEntry| {
            Ok(Arc::new(
                crate::rules::evaluators::plan_caps::TradingHoursRule::from_entry(e),
            ))
        },
        "margin" => |e: &RuleEntry| {
            Ok(Arc::new(
                crate::rules::evaluators::plan_caps::MarginRule::from_entry(e),
            ))
        },
        _ => return None,
    })
}

/// **P0.4 fix**: resolves the default factory for each pack kind and
/// returns a typed error listing every kind the engine cannot enforce.
/// Silently dropping a rule means silently not enforcing it — a tenant
/// admin must know when their pack references something this engine
/// version does not support.
pub fn default_factories_for_kinds(
    kind_names: &[&str],
) -> Result<Vec<(String, RuleFactory)>, Error> {
    let mut out = Vec::with_capacity(kind_names.len());
    let mut unsupported: Vec<&str> = Vec::new();
    for k in kind_names {
        match default_factory_for_kind(k) {
            Some(f) => out.push(((*k).to_string(), f)),
            None => unsupported.push(k),
        }
    }
    if !unsupported.is_empty() {
        return Err(Error::invalid_config(format!(
            "rule pack references unsupported rule kind(s): {} — \
             these rules would NOT be enforced; fix the pack or upgrade the engine",
            unsupported.join(", ")
        )));
    }
    Ok(out)
}

/// Default rule library (registered automatically).
#[must_use]
pub fn default_rules() -> Vec<Arc<dyn Rule>> {
    use crate::rules::evaluators::{
        consistency, cooldown, copy_trading, daily_drawdown, grid_trading, hedging, hft_scalping,
        inactivity, max_daily_trades, max_drawdown, max_open_positions, max_position_size,
        min_trading_days, news_trading, overnight_holding, per_trade_max_loss, plan_caps,
        profit_target, sl_required, time_limit, tp_required, trailing_drawdown, weekend_holding,
    };
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
        // P2.15: new rules added in the industry-semantics upgrade.
        // P0.1: both default to DISABLED — each rule's `is_enabled`
        // requires an explicit plan flag (`hft_ban_enabled`,
        // `per_trade_max_loss_pct`) or an enabling pack entry.
        Arc::new(hft_scalping::HftScalpingRule::default()),
        Arc::new(per_trade_max_loss::PerTradeMaxLossRule::default()),
        // P1.4/P1.5: inactivity termination (unlimited-time programs).
        Arc::new(inactivity::InactivityRule::default()),
        // §C.2: previously-unenforced plan fields now enforced.
        Arc::new(plan_caps::MaxTotalLotsRule::default()),
        Arc::new(plan_caps::TradingHoursRule),
        Arc::new(plan_caps::MarginRule),
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
    #[must_use]
    pub fn empty() -> Self {
        RuleRegistry {
            rules: Vec::new(),
            by_id: HashMap::new(),
        }
    }

    /// Builds a registry pre-populated with the default rule library.
    #[must_use]
    pub fn with_default_rules() -> Self {
        let mut r = Self::empty();
        for rule in default_rules() {
            r.register(rule);
        }
        r
    }

    /// **P0.2 fix**: builds the default rule library with plan-disabled
    /// opt-in rules removed entirely. Each remaining rule still gates
    /// itself via `is_enabled` (defense in depth), but rules the plan
    /// cannot enable are not registered at all — they cannot run, and
    /// `registry.all()` reflects the effective rule set.
    #[must_use]
    pub fn with_default_rules_for_plan(plan: &crate::config::plan::ChallengePlan) -> Self {
        let hft_id = crate::core::ids::RuleId::named("hft_scalping");
        let per_trade_id = crate::core::ids::RuleId::named("per_trade_max_loss");
        let inactivity_id = crate::core::ids::RuleId::named("inactivity");
        let hedging_id = crate::core::ids::RuleId::named("hedging");
        let grid_id = crate::core::ids::RuleId::named("grid_trading");
        let copy_id = crate::core::ids::RuleId::named("copy_trading");
        let news_id = crate::core::ids::RuleId::named("news_trading");
        let cooldown_id = crate::core::ids::RuleId::named("cooldown");
        let mut r = Self::empty();
        for rule in default_rules() {
            // Drop the opt-in rules the plan does not enable.
            let id = rule.id();
            if id == hft_id && !plan.hft_ban_enabled {
                continue;
            }
            if id == per_trade_id
                && plan.per_trade_max_loss_pct.is_none()
                && plan.per_trade_max_loss_money.is_none()
            {
                continue;
            }
            if id == inactivity_id && plan.inactivity_days.is_none() {
                continue;
            }
            // P0.2: drop the prohibition rules the plan disables
            // (plan allows the behavior ⇒ no rule to enforce).
            if id == hedging_id && plan.hedging_allowed {
                continue;
            }
            if id == grid_id && plan.grid_trading_allowed {
                continue;
            }
            if id == copy_id && plan.copy_trading_allowed {
                continue;
            }
            if id == news_id && plan.news_trading_allowed {
                continue;
            }
            if id == cooldown_id && plan.cooldown_seconds == 0 {
                continue;
            }
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

    /// **P1-6 fix, P0-D fix**: builds a registry from a [`RulePack`]
    /// (versioned data). For each `RuleEntry` in the pack, look up the
    /// rule factory by `kind`, pass the *entry* to the factory (so the
    /// rule can read `value`, `basis`, `unit`, `tolerance_cents`,
    /// `priority`, etc.), and register the resulting parameterized rule.
    ///
    /// The rule's `evaluate` reads from its own `params` field
    /// (populated from the entry) instead of from `ctx.account.plan` —
    /// so a tenant editing the pack through the form actually changes
    /// the verdict. This is the binding spec's EVL-01/02 requirement.
    ///
    /// Disabled entries (`enabled: false`) are skipped.
    ///
    /// **P0.4 fix**: unknown kinds now FAIL with a typed
    /// [`Error::InvalidConfig`] listing the unsupported kind — silently
    /// dropping a rule means silently not enforcing it.
    pub fn build_from_pack(pack: &crate::rulepack::RulePack) -> Result<Self, Error> {
        let mut registry = Self::empty();
        for entry in &pack.rules {
            if !entry.enabled {
                continue;
            }
            let factory = default_factory_for_kind(&entry.kind).ok_or_else(|| {
                Error::invalid_config(format!(
                    "rule pack {} references unsupported rule kind '{}' — \
                     the rule would silently not be enforced",
                    pack.id, entry.kind
                ))
            })?;
            let rule = factory(entry)?;
            registry.register(rule);
        }
        Ok(registry)
    }

    /// Returns the rule with the given id, if any.
    #[must_use]
    pub fn get(&self, id: RuleId) -> Option<&Arc<dyn Rule>> {
        self.by_id.get(&id).map(|&i| &self.rules[i])
    }

    /// Returns the list of all rules.
    #[must_use]
    pub fn all(&self) -> &[Arc<dyn Rule>] {
        &self.rules
    }

    /// Returns the rules that apply to the given context kind / scope.
    #[must_use]
    pub fn applicable(&self, kind: RuleContextKind) -> Vec<&Arc<dyn Rule>> {
        let scope = match kind {
            RuleContextKind::OnOrderSubmit => EvaluationScope::PreTrade,
            RuleContextKind::OnTradeFill => EvaluationScope::PostTrade,
            RuleContextKind::OnTick => EvaluationScope::OnTick,
            RuleContextKind::OnDayRollover | RuleContextKind::OnEndOfDay => {
                EvaluationScope::Periodic
            }
            RuleContextKind::OnDemand => EvaluationScope::OnDemand,
        };
        self.rules
            .iter()
            .filter(|r| r.scope() == scope || r.scope() == EvaluationScope::OnDemand)
            .collect()
    }

    /// Evaluates all rules applicable to the given context, returning reports.
    ///
    /// **P3.20 fix**: each rule's `evaluate` is wrapped in
    /// `std::panic::catch_unwind`. A panicking rule (e.g. a divide-by-zero
    /// in some edge case) is caught and converted to a `RuleVerdict::Warn`
    /// with a descriptive message — the other rules still run, and the
    /// process stays alive. Critical because the engine serves many
    /// accounts on one process; one bad rule shouldn't take down
    /// everyone's evaluation.
    pub fn evaluate(&self, ctx: &RuleContext) -> crate::Result<Vec<RuleReport>> {
        let mut reports = Vec::with_capacity(self.rules.len());
        for rule in &self.rules {
            if !rule.is_enabled(ctx) {
                continue;
            }
            let scope = rule.scope();
            let applicable = match ctx.kind {
                RuleContextKind::OnOrderSubmit => {
                    scope == EvaluationScope::PreTrade || scope == EvaluationScope::OnDemand
                }
                RuleContextKind::OnTradeFill => {
                    scope == EvaluationScope::PostTrade || scope == EvaluationScope::OnDemand
                }
                RuleContextKind::OnTick => {
                    scope == EvaluationScope::OnTick || scope == EvaluationScope::OnDemand
                }
                RuleContextKind::OnDayRollover | RuleContextKind::OnEndOfDay => {
                    scope == EvaluationScope::Periodic || scope == EvaluationScope::OnDemand
                }
                RuleContextKind::OnDemand => true,
            };
            if !applicable {
                continue;
            }
            // P3.20: catch panics from buggy rules. A panic is converted
            // to a Warn verdict so the process keeps running.
            let verdict_result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rule.evaluate(ctx)));
            let verdict = match verdict_result {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    let base = Violation::new(
                        ctx.account.id,
                        rule.id(),
                        rule.name(),
                        rule.kind(),
                        ViolationSeverity::Warning,
                        format!("rule evaluation error: {e}"),
                        ctx.server_time.ts(),
                    )
                    .with_tenant(ctx.account.tenant_id);
                    Self::failure_verdict(rule, base)
                }
                Err(panic_payload) => {
                    let panic_msg = if let Some(s) = panic_payload.downcast_ref::<&'static str>() {
                        (*s).to_string()
                    } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                        s.clone()
                    } else {
                        "unknown panic payload".to_string()
                    };
                    let base = Violation::new(
                        ctx.account.id,
                        rule.id(),
                        rule.name(),
                        rule.kind(),
                        ViolationSeverity::Warning,
                        format!(
                            "rule panicked (caught by registry, process preserved): {panic_msg}"
                        ),
                        ctx.server_time.ts(),
                    )
                    .with_tenant(ctx.account.tenant_id);
                    Self::failure_verdict(rule, base)
                }
            };
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

    fn failure_verdict(rule: &Arc<dyn Rule>, base: Violation) -> crate::rules::traits::RuleVerdict {
        use crate::rules::traits::RuleVerdict;
        let policy = rule
            .params()
            .and_then(|p| p.failure_policy.as_deref())
            .unwrap_or("warn");
        match policy {
            "soft_fail" => RuleVerdict::SoftFail(base),
            "fail" => RuleVerdict::Fail(base),
            "gap_flagged" => RuleVerdict::GapFlagged(base),
            _ => RuleVerdict::Warn(base),
        }
    }
}

impl std::fmt::Display for RuleContextKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use RuleContextKind::{
            OnDayRollover, OnDemand, OnEndOfDay, OnOrderSubmit, OnTick, OnTradeFill,
        };
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
