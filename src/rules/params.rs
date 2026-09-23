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
//!
//! **P0.3 fix**: `RuleParams` now carries the entry's [`RuleUnit`].
//! Data-driven rules must interpret `value` *through the unit* via
//! [`RuleParams::effective_money`] / [`RuleParams::effective_pct`]:
//! `Percent` means "fraction of the reference" while `Money` means "an
//! absolute amount". A pack entry `{unit: money, value: 5000}` on a
//! $100k account is a $5,000 limit — NOT `5000 × reference`.

use crate::rulepack::{RuleEntry, RuleUnit};

/// The reference amount used to interpret a `Percent`-unit pack value.
/// `None` means the caller had no sensible reference (e.g. no
/// pending order yet) — `Money`-unit values are still usable.
#[derive(Debug, Clone, Copy)]
pub struct LimitContext<'a> {
    /// Account initial balance (percent of initial balance).
    pub initial_balance: crate::core::types::Money,
    /// Current balance (percent of current balance).
    pub balance: crate::core::types::Money,
    /// Optional pending order quantity — used by quantity-type rules
    /// (max_position_size, max_total_lots) where `Percent` is
    /// interpreted as a fraction of the account's initial balance
    /// expressed in lots, and `Money` as an absolute lot count.
    pub order_quantity: Option<&'a crate::core::types::Quantity>,
}

impl<'a> LimitContext<'a> {
    /// Builds a limit context from an account (no order quantity).
    #[must_use]
    pub fn from_account(account: &'a crate::core::account::Account) -> Self {
        LimitContext {
            initial_balance: account.initial_balance,
            balance: account.balance,
            order_quantity: None,
        }
    }
}

/// Parameters extracted from a [`RuleEntry`]. Stored on each
/// parameterized rule; `evaluate` reads from this when `Some`, else
/// falls back to `ctx.account.plan.*`.
#[derive(Debug, Clone, Default)]
pub struct RuleParams {
    /// Numeric value from the entry (interpreted per `unit`).
    pub value: Option<rust_decimal::Decimal>,
    /// **P0.3 fix**: unit of `value` (percent of a reference, or an
    /// absolute money amount). Copied from the pack entry; `None` for
    /// rules constructed via `Default` (plan fallback governs).
    pub unit: Option<RuleUnit>,
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
    pub params_json: String,
    pub severity: Option<String>,
    /// Optional per-rule failure policy from the pack entry.
    pub failure_policy: Option<String>,
}

impl RuleParams {
    /// Constructs params from a `RuleEntry`. Each field is read from
    /// the entry; missing fields stay `None` (rule falls back to plan).
    #[must_use]
    pub fn from_entry(e: &RuleEntry) -> Self {
        RuleParams {
            value: Some(e.value),
            unit: Some(e.unit),
            basis: match e.basis {
                crate::rulepack::RuleBasis::Static => {
                    Some(crate::config::plan::LossReference::Static)
                }
                crate::rulepack::RuleBasis::Trailing => {
                    Some(crate::config::plan::LossReference::Trailing)
                }
                crate::rulepack::RuleBasis::EodTrailing => {
                    Some(crate::config::plan::LossReference::EodTrailing)
                }
            },
            tolerance_cents: e.tolerance_cents,
            priority: Some(e.priority),
            early_warning_pct: e.early_warning_pct,
            enabled: e.enabled,
            params_json: e.params_json.clone(),
            severity: e.severity.clone(),
            failure_policy: e.failure_policy.clone(),
        }
    }

    /// Returns the entry's `value` if this params was populated from a
    /// pack entry; otherwise `None` (rule must fall back to plan).
    #[must_use]
    pub fn value(&self) -> Option<rust_decimal::Decimal> {
        self.value
    }

    /// Returns the entry's `unit` if populated; otherwise `None`.
    #[must_use]
    pub fn unit(&self) -> Option<RuleUnit> {
        self.unit
    }

    /// Returns the entry's `basis` if populated; otherwise `None`.
    #[must_use]
    pub fn basis(&self) -> Option<crate::config::plan::LossReference> {
        self.basis
    }

    /// Returns the entry's `tolerance_cents` if populated; otherwise `None`.
    #[must_use]
    pub fn tolerance_cents(&self) -> Option<i64> {
        self.tolerance_cents
    }

    /// Returns the entry's `priority` if populated; otherwise `None`.
    #[must_use]
    pub fn priority(&self) -> Option<u32> {
        self.priority
    }

    /// Returns the entry's `early_warning_pct` if populated; otherwise `None`.
    #[must_use]
    pub fn early_warning_pct(&self) -> Option<rust_decimal::Decimal> {
        self.early_warning_pct
    }

    /// **P0.3 / A.1 fix**: interprets the entry's `value` according to
    /// its `unit` and returns the effective limit as `Option<Money>`.
    ///
    /// - `None` value → `Ok(None)` — the caller **must** fall back to
    ///   the plan-derived limit. Returning `Ok(reference)` here was the
    ///   original fail-open bug: a caller that forgot to guard received
    ///   a limit equal to 100% of the reference — effectively "never
    ///   breach".
    /// - `Percent` → `Ok(Some(value × reference))`.
    /// - `Money` → `Ok(Some(value))`.
    /// - Unknown unit → `Err` (fail closed — a mis-encoded pack must
    ///   never silently produce a limit that can never breach).
    ///
    /// **Contract**: every caller of `effective_money` **must** handle
    /// the `Ok(None)` case by falling back to the plan. Callers that
    /// treat `None` as "no limit configured" (pass) are correct for
    /// rules that are optional; callers that treat it as "use plan
    /// default" are correct for rules that are always present.
    pub fn effective_money(
        &self,
        unit_context: &str,
        reference: crate::core::types::Money,
    ) -> Result<Option<crate::core::types::Money>, crate::core::Error> {
        let Some(v) = self.value else {
            // A.1 fix: no pack value → caller falls back to plan.
            // Returning the reference here was fail-open (100% limit =
            // never breach). Now the caller must handle `None` explicitly.
            return Ok(None);
        };
        match self.unit {
            Some(RuleUnit::Percent) => Ok(Some(crate::core::types::Money(v * reference.0))),
            Some(RuleUnit::Money) => Ok(Some(crate::core::types::Money(v))),
            None => Ok(Some(crate::core::types::Money(v * reference.0))),
            #[allow(unreachable_patterns)]
            Some(_) => Err(crate::core::Error::invalid_config(format!(
                "rule-pack entry: unit '{}' cannot be interpreted for {unit_context} \
                 (expected percent or money) — refusing to evaluate (fail closed)",
                self.unit.map_or("<none>".to_string(), |u| u.to_string())
            ))),
        }
    }

    /// **P0.3 fix**: interprets the entry's `value` according to its
    /// `unit` and returns the effective limit as a bare fraction
    /// (`Percent` → the fraction itself; `Money` → the amount divided
    /// by `reference`, so callers that compute in percent-space still
    /// honour a money-unit pack entry).
    ///
    /// Rules whose value is naturally a count or a quantity (not
    /// money) should use [`RuleParams::effective_count`] instead.
    pub fn effective_pct(
        &self,
        unit_context: &str,
        reference: crate::core::types::Money,
    ) -> Result<rust_decimal::Decimal, crate::core::Error> {
        let Some(v) = self.value else {
            return Err(crate::core::Error::invalid_config(format!(
                "{unit_context}: no pack value present — caller must fall back to plan"
            )));
        };
        match self.unit {
            Some(RuleUnit::Percent) | None => Ok(v),
            Some(RuleUnit::Money) => {
                if reference.0.is_zero() {
                    return Err(crate::core::Error::invalid_config(format!(
                        "{unit_context}: money-unit value {v} cannot be normalized against a zero reference"
                    )));
                }
                Ok(v / reference.0)
            }
            #[allow(unreachable_patterns)]
            Some(_) => Err(crate::core::Error::invalid_config(format!(
                "rule-pack entry: unit '{}' cannot be interpreted for {unit_context} \
                 (expected percent or money) — refusing to evaluate (fail closed)",
                self.unit.map_or("<none>".to_string(), |u| u.to_string())
            ))),
        }
    }

    /// Extracts a decimal-valued key from the entry's `params_json`
    /// (e.g. `{"cv_threshold": 0.05}` → `0.05`). Used by rules with
    /// secondary thresholds that don't fit the single `value` slot
    /// (grid CV threshold, copy-trading window, …).
    ///
    /// Requires the `serialization` feature to parse JSON; without it
    /// this returns `None` and callers fall back to their documented
    /// defaults (documented limitation, not silent misconfiguration —
    /// the primary `value` slot is still honoured on every build).
    #[must_use]
    pub fn json_decimal(&self, key: &str) -> Option<rust_decimal::Decimal> {
        #[cfg(feature = "serialization")]
        {
            let v: serde_json::Value = serde_json::from_str(&self.params_json).ok()?;
            let n = v.get(key)?.as_f64()?;
            rust_decimal::Decimal::try_from(n).ok()
        }
        #[cfg(not(feature = "serialization"))]
        {
            let _ = key;
            None
        }
    }

    /// **P0.3 fix**: interprets the entry's `value` for rules whose
    /// limit is a *count* (min_trading_days, max_open_positions,
    /// max_daily_trades) or a *quantity* (max_position_size,
    /// max_total_lots, cooldown seconds, min round-trip seconds).
    ///
    /// `Count`-style values are only meaningful with the default
    /// `Percent` unit slot (which these packs use as "raw number") or
    /// a `Money` unit (absolute count). Anything else fails closed.
    pub fn effective_count(
        &self,
        unit_context: &str,
    ) -> Result<rust_decimal::Decimal, crate::core::Error> {
        let Some(v) = self.value else {
            return Err(crate::core::Error::invalid_config(format!(
                "{unit_context}: no pack value present — caller must fall back to plan"
            )));
        };
        match self.unit {
            // Count-type entries historically ride on the default
            // `Percent` encoding (value = raw number), and `Money` is
            // accepted as an explicit "absolute number" encoding.
            Some(RuleUnit::Percent) | Some(RuleUnit::Money) | None => Ok(v),
        }
    }
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
