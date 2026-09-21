//! News trading rule.
//!
//! Many prop firms forbid opening or closing positions around scheduled
//! high-impact news events (e.g. NFP, FOMC, CPI).
//!
//! **Calendar source (§A.2 fix — documented limitation)**: the calendar is
//! the private, hardcoded [`builtin_events`] list in this module. It is
//! **not pluggable** — the pluggable calendar trait this doc once
//! referenced was removed. Production deployments that need a live
//! calendar feed must extend `builtin_events` (or replace this rule via
//! the registry); there is no configuration seam.

use crate::core::ids::RuleId;
use crate::core::violation::{ViolationKind, ViolationSeverity};
use crate::rules::context::{EvaluationScope, RuleContext};
use crate::rules::params::{ParameterizedRule, RuleParams};
use crate::rules::registry::build_violation;
use crate::rules::traits::{Rule, RuleVerdict};
use chrono::{Datelike, NaiveTime, TimeZone, Utc, Weekday};

#[derive(Debug, Clone, Default)]
pub struct NewsTradingRule {
    /// **P0-D fix**: pack-derived parameters. When `Some`, rule reads
    /// `value`/`basis`/`tolerance_cents`/`priority` from here instead of
    /// from `ctx.account.plan` — so a tenant editing the pack actually
    /// changes the verdict. When `None` (constructed via `Default`), the
    /// rule falls back to plan-derived config.
    pub params: Option<RuleParams>,
}

/// A high-impact news event. (Illustrative only.)
#[derive(Debug, Clone)]
pub struct NewsEvent {
    pub name: &'static str,
    pub weekday: Weekday,
    pub hour: u32,
    pub minute: u32,
}

/// Returns the built-in list of recurring news events. **Hardcoded and
/// not pluggable** (§A.2): there is no calendar-provider seam; the
/// pluggable API this list once promised was removed.
#[must_use]
pub fn builtin_events() -> Vec<NewsEvent> {
    vec![
        NewsEvent {
            name: "NFP",
            weekday: Weekday::Fri,
            hour: 12,
            minute: 30,
        },
        NewsEvent {
            name: "CPI",
            weekday: Weekday::Wed,
            hour: 12,
            minute: 30,
        },
        NewsEvent {
            name: "FOMC",
            weekday: Weekday::Wed,
            hour: 18,
            minute: 0,
        },
        NewsEvent {
            name: "ECB Rate",
            weekday: Weekday::Thu,
            hour: 11,
            minute: 45,
        },
        NewsEvent {
            name: "BOE Rate",
            weekday: Weekday::Thu,
            hour: 11,
            minute: 0,
        },
    ]
}

/// Returns true if the given timestamp falls within `window_minutes` of a
/// high-impact news event.
#[must_use]
pub fn within_news_window(
    ts: chrono::DateTime<chrono::Utc>,
    window_minutes: i64,
) -> Option<NewsEvent> {
    let events = builtin_events();
    for e in &events {
        // Construct this week's instance of the event.
        let naive_time = NaiveTime::from_hms_opt(e.hour, e.minute, 0)?;
        let today = ts.weekday();
        // Number of days since Monday for each weekday.
        let event_offset = i64::from(e.weekday.num_days_from_monday());
        let today_offset = i64::from(today.num_days_from_monday());
        let day_diff = (event_offset - today_offset).rem_euclid(7);
        let event_date = (ts + chrono::Duration::days(day_diff)).date_naive();
        let event_dt = event_date.and_time(naive_time);
        let event_utc = Utc.from_utc_datetime(&event_dt);
        let delta = (ts - event_utc).num_minutes().abs();
        if delta <= window_minutes {
            return Some(e.clone());
        }
    }
    None
}

impl Rule for NewsTradingRule {
    fn id(&self) -> RuleId {
        RuleId::named("news_trading")
    }
    fn name(&self) -> &'static str {
        "News Trading"
    }
    fn kind(&self) -> ViolationKind {
        ViolationKind::NewsTrading
    }
    fn scope(&self) -> EvaluationScope {
        EvaluationScope::PreTrade
    }
    fn severity(&self) -> ViolationSeverity {
        ViolationSeverity::Hard
    }

    fn description(&self) -> &'static str {
        "Restricts trading around scheduled high-impact news events."
    }

    fn is_enabled(&self, ctx: &RuleContext) -> bool {
        // P0.4: pack entry's enabled flag overrides the plan. A news
        // pack entry means "news trading forbidden" when enabled.
        if let Some(p) = &self.params {
            return p.enabled && !ctx.account.plan.news_trading_allowed;
        }
        !ctx.account.plan.news_trading_allowed
    }

    fn evaluate(&self, ctx: &RuleContext) -> crate::Result<RuleVerdict> {
        if ctx.account.plan.news_trading_allowed {
            return Ok(RuleVerdict::Pass);
        }
        let Some(order) = &ctx.pending_order else {
            return Ok(RuleVerdict::Pass);
        };
        let window = ctx
            .rule_config
            .news_trading
            .as_ref()
            .map_or(2, |c| i64::from(c.window_minutes));
        if let Some(event) = within_news_window(order.submitted_at, window) {
            let v = build_violation(
                self,
                ctx,
                ViolationSeverity::Hard,
                format!(
                    "Order submitted within {}min of {} news event",
                    window, event.name
                ),
            );
            return Ok(RuleVerdict::Fail(v));
        }
        Ok(RuleVerdict::Pass)
    }
}

impl NewsTradingRule {
    /// Constructs a parameterized rule from a pack entry (P0-D fix).
    #[must_use]
    pub fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        NewsTradingRule {
            params: Some(RuleParams::from_entry(entry)),
        }
    }
}

impl ParameterizedRule for NewsTradingRule {
    fn from_entry(entry: &crate::rulepack::RuleEntry) -> Self {
        NewsTradingRule::from_entry(entry)
    }
}
