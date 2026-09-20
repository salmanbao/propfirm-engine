//! Rule parameterization helpers (P0-D fix).
//!
//! Each parameterized rule holds a [`RuleParams`] struct populated from
//! the [`RuleEntry`](crate::rulepack::RuleEntry). The rule's `evaluate`
//! reads from `self.params` (when set) instead of from
//! `ctx.account.plan` — so a tenant editing the rule pack through the
//! form actually changes the verdict (the binding spec's EVL-01/02
//! requirement).
//!
//! `RuleParams` is `Option`-backed so rules constructed via `Default`
//! (e.g. by `default_rules()`) fall back to plan-derived config —
//! backwards-compatible with the existing pipeline path.

use crate::rulepack::RuleEntry;

/// Parameters extracted from a [`RuleEntry`]. Stored on each
/// parameterized rule; `evaluate` reads from this when `Some`, else
/// falls back to `ctx.account.plan.*`.
#[derive(Debug, Clone, Default)]
pub struct RuleParams {
    /// Numeric value from the entry (interpreted per `unit`).
    pub value: Option<rust_decimal::Decimal>,
    /// Measurement basis (static or trailing).
    pub basis: Option<crate::config::plan::LossReference>,
    /// Tolerance in cents (overrides `Rule::tolerance_cents()`).
    pub tolerance_cents: Option<i64>,
    /// Explicit priority (overrides `Rule::priority()`).
    pub priority: Option<u32>,
    /// Early-warning threshold as a fraction of `value` (e.g. 0.80).
    pub early_warning_pct: Option<rust_decimal::Decimal>,
    /// Whether the rule is enabled.
    pub enabled: bool,
    /// Free-form params JSON (rule-kind-specific).
    pub params_json: String,
}

impl RuleParams {
    /// Constructs params from a `RuleEntry`. Each field is read from
    /// the entry; missing fields stay `None` (rule falls back to plan).
    pub fn from_entry(e: &RuleEntry) -> Self {
        RuleParams {
            value: Some(e.value),
            basis: match e.basis {
                crate::rulepack::RuleBasis::Static => Some(crate::config::plan::LossReference::Static),
                crate::rulepack::RuleBasis::Trailing => Some(crate::config::plan::LossReference::Trailing),
                crate::rulepack::RuleBasis::EodTrailing => Some(crate::config::plan::LossReference::EodTrailing),
            },
            tolerance_cents: e.tolerance_cents,
            priority: Some(e.priority),
            early_warning_pct: e.early_warning_pct,
            enabled: e.enabled,
            params_json: e.params_json.clone(),
        }
    }
    /// Returns the entry's `value` if this params was populated from a
    /// pack entry; otherwise `None` (rule must fall back to plan).
    pub fn value(&self) -> Option<rust_decimal::Decimal> { self.value }

    /// Returns the entry's `basis` if populated; otherwise `None`.
    pub fn basis(&self) -> Option<crate::config::plan::LossReference> { self.basis }

    /// Returns the entry's `tolerance_cents` if populated; otherwise `None`.
    pub fn tolerance_cents(&self) -> Option<i64> { self.tolerance_cents }

    /// Returns the entry's `priority` if populated; otherwise `None`.
    pub fn priority(&self) -> Option<u32> { self.priority }

    /// Returns the entry's `early_warning_pct` if populated; otherwise `None`.
    pub fn early_warning_pct(&self) -> Option<rust_decimal::Decimal> { self.early_warning_pct }
}

/// Trait for rules that can be constructed from a `RuleEntry`.
/// Implement `from_entry` for each parameterized rule; the registry's
/// `build_from_pack` calls this to wire pack data into rule instances.
pub trait ParameterizedRule: Rule {
    /// Constructs a parameterized rule from a pack entry. The rule
    /// stores the entry's `value`/`basis`/`tolerance_cents`/etc. in a
    /// `RuleParams` field; `evaluate` reads from there instead of from
    /// `ctx.account.plan`.
    fn from_entry(entry: &RuleEntry) -> Self
    where
        Self: Sized;
}

use crate::rules::traits::Rule;
