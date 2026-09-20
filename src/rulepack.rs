//! Rule pack as versioned data (P1-6 fix).
//!
//! The platform spec requires rule packs to be **versioned data**,
//! editable by tenant admins through a form (EVL-01/02), bound to an
//! account at purchase, and re-bindable only via an explicit, audited
//! action (EVL-34) — never by recompiling and redeploying the engine.
//!
//! This module defines the serializable schema. It maps directly to
//! `alpha-one/docs/09-evaluation-engine.md` §3.1:
//!
//! ```json
//! {
//!   "id": "funderblu-default-v3",
//!   "version": 3,
//!   "lifecycle": "active",        // draft | active | superseded
//!   "effective_from": "2026-09-01T00:00:00Z",
//!   "rules": [
//!     {
//!       "id": "max_total_loss",
//!       "kind": "max_drawdown",
//!       "basis": "static",        // static | trailing
//!       "unit": "percent",        // percent | money
//!       "value": 0.10,
//!       "tolerance_cents": 1,
//!       "early_warning_pct": 0.80,
//!       "priority": 1000
//!     },
//!     ...
//!   ]
//! }
//! ```
//!
//! At request time, `RuleRegistry::build_from_pack(&pack)` interprets
//! this data and produces the active rule set. The current rule
//! *implementations* (`DailyDrawdownRule`, `MaxDrawdownRule`, etc.)
//! become the code behind each `kind` — re-parameterized from data
//! instead of from compiled struct fields.

use crate::core::ids::RuleId;
use crate::core::types::{Money, Pct, Timestamp};
use crate::core::Error;

/// Lifecycle of a rule pack. Maps to the binding spec's
/// `lifecycle: draft | active | superseded` field.
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[cfg_attr(feature = "serialization", serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PackLifecycle {
    /// Pack is being edited by tenant admins. Not yet bound to any
    /// account; cannot be used for evaluation.
    Draft,
    /// Pack is live and bound to accounts. Used for evaluation.
    Active,
    /// Pack has been replaced by a newer `Active` version. Preserved for
    /// historical replay/dispute resolution; not bound to new accounts.
    Superseded,
}

impl std::fmt::Display for PackLifecycle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackLifecycle::Draft => write!(f, "draft"),
            PackLifecycle::Active => write!(f, "active"),
            PackLifecycle::Superseded => write!(f, "superseded"),
        }
    }
}

impl std::str::FromStr for PackLifecycle {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "draft" => Ok(PackLifecycle::Draft),
            "active" => Ok(PackLifecycle::Active),
            "superseded" => Ok(PackLifecycle::Superseded),
            other => Err(Error::invalid_config(format!(
                "unknown pack lifecycle '{other}' (expected draft/active/superseded)"
            ))),
        }
    }
}

/// Basis of a rule's measurement. Maps directly to the binding spec's
/// `basis: static | trailing | eod_trailing` field.
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[cfg_attr(feature = "serialization", serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleBasis {
    /// Measured from a fixed reference point (e.g. initial balance).
    Static,
    /// Measured from a high-water mark that floats up intraday.
    Trailing,
    /// **P1.6 fix**: Measured from prior day's closing balance; floor
    /// resets once per day at the trading-session rollover.
    EodTrailing,
}

impl std::fmt::Display for RuleBasis {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuleBasis::Static => write!(f, "static"),
            RuleBasis::Trailing => write!(f, "trailing"),
            RuleBasis::EodTrailing => write!(f, "eod_trailing"),
        }
    }
}

impl std::str::FromStr for RuleBasis {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "static" => Ok(RuleBasis::Static),
            "trailing" => Ok(RuleBasis::Trailing),
            "eod_trailing" | "eodtrailing" | "eod-trailing" => Ok(RuleBasis::EodTrailing),
            other => Err(Error::invalid_config(format!(
                "unknown rule basis '{other}' (expected static/trailing/eod_trailing)"
            ))),
        }
    }
}

/// Unit of a rule's value.
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[cfg_attr(feature = "serialization", serde(rename_all = "snake_case"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleUnit {
    /// Value is a fraction of the reference (0.10 = 10%).
    Percent,
    /// Value is an absolute money amount.
    Money,
}

impl std::fmt::Display for RuleUnit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RuleUnit::Percent => write!(f, "percent"),
            RuleUnit::Money => write!(f, "money"),
        }
    }
}

impl std::str::FromStr for RuleUnit {
    type Err = Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "percent" => Ok(RuleUnit::Percent),
            "money" => Ok(RuleUnit::Money),
            other => Err(Error::invalid_config(format!(
                "unknown rule unit '{other}' (expected percent/money)"
            ))),
        }
    }
}

/// A single rule entry in a [`RulePack`]. This is the data shape — the
/// concrete rule implementation (e.g. `MaxDrawdownRule`) is looked up
/// by `kind` at evaluation time.
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[derive(Debug, Clone)]
pub struct RuleEntry {
    /// Stable identifier (e.g. "`max_total_loss`", "`daily_loss`", "`profit_target`").
    pub id: String,
    /// Kind name — maps to a registered rule factory.
    pub kind: String,
    /// Measurement basis (static or trailing).
    pub basis: RuleBasis,
    /// Unit of `value` (percent or money).
    pub unit: RuleUnit,
    /// The numeric value (interpreted per `unit`).
    pub value: rust_decimal::Decimal,
    /// Optional tolerance, in cents, to absorb broker rounding noise at
    /// the exact boundary (P2 fix — binding spec's `tolerance_cents`,
    /// default 1).
    pub tolerance_cents: Option<i64>,
    /// Optional early-warning threshold as a fraction of `value`
    /// (0.80 = "warn at 80% of breach threshold"). P1-13 fix.
    pub early_warning_pct: Option<rust_decimal::Decimal>,
    /// Explicit numeric priority (P0-4 fix). Higher = wins.
    pub priority: u32,
    /// Whether this rule is enabled in this pack.
    pub enabled: bool,
    /// Free-form parameters specific to the rule kind, stored as a JSON
    /// string (the engine never inspects this; the rule implementation
    /// parses it itself).
    pub params_json: String,
}

impl RuleEntry {
    /// Constructs a new rule entry with sensible defaults.
    pub fn new(
        id: impl Into<String>,
        kind: impl Into<String>,
        value: rust_decimal::Decimal,
    ) -> Self {
        RuleEntry {
            id: id.into(),
            kind: kind.into(),
            basis: RuleBasis::Static,
            unit: RuleUnit::Percent,
            value,
            tolerance_cents: Some(1),
            early_warning_pct: Some(rust_decimal::Decimal::new(8, 1)), // 0.8
            priority: 100,
            enabled: true,
            params_json: "{}".into(),
        }
    }
}

/// A versioned rule pack. The full data artifact that defines which
/// rules apply to an account and with what parameters.
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[derive(Debug, Clone)]
pub struct RulePack {
    /// Stable pack identifier (e.g. "funderblu-default-v3").
    pub id: String,
    /// Monotonically increasing version number.
    pub version: u32,
    /// Tenant this pack belongs to (P1-9 fix).
    pub tenant_id: crate::tenant::TenantId,
    /// Lifecycle stage.
    pub lifecycle: PackLifecycle,
    /// When this pack becomes effective.
    pub effective_from: Timestamp,
    /// Optional superseded-by pointer (the pack id that replaced this one).
    pub superseded_by: Option<String>,
    /// Human-readable description.
    pub description: String,
    /// The rules in this pack, in evaluation order (priority still wins
    /// over registration order — see P0-4).
    pub rules: Vec<RuleEntry>,
    /// Initial balance this pack is configured for (used for percent→money conversion).
    pub initial_balance: Money,
    /// Maximum leverage (e.g. 100).
    pub leverage: u32,
    /// Profit target as a percentage (0.10 = 10%).
    pub profit_target_pct: Pct,
}

impl RulePack {
    /// Validates the pack for internal consistency.
    pub fn validate(&self) -> Result<(), Error> {
        if self.id.is_empty() {
            return Err(Error::invalid_config("pack id cannot be empty"));
        }
        if self.rules.is_empty() {
            return Err(Error::invalid_config(format!(
                "pack {} has no rules",
                self.id
            )));
        }
        // Check for duplicate rule ids within the same pack.
        let mut seen = std::collections::HashSet::new();
        for r in &self.rules {
            if !seen.insert(&r.id) {
                return Err(Error::invalid_config(format!(
                    "pack {} has duplicate rule id '{}'",
                    self.id, r.id
                )));
            }
        }
        Ok(())
    }

    /// Returns the rule entry with the given id, if any.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&RuleEntry> {
        self.rules.iter().find(|r| r.id == id)
    }

    /// Computes the real sha256 content hash of this pack's content
    /// (P0-B fix — was previously mislabeled `SipHash` truncated to 64 bits).
    ///
    /// Used by the pure-evaluate function (P1-7 fix) to record the exact
    /// rule set that produced a verdict — so any past verdict can be
    /// recomputed byte-for-byte from its recorded inputs.
    #[must_use]
    pub fn content_hash(&self) -> String {
        use crate::sha256_helper::Sha256Hasher;
        use std::hash::Hash;
        let mut h = Sha256Hasher::new();
        // Hash the pack's identifying content (not the volatile metadata).
        self.id.hash(&mut h);
        self.version.hash(&mut h);
        self.tenant_id.hash(&mut h);
        for r in &self.rules {
            r.id.hash(&mut h);
            r.kind.hash(&mut h);
            r.basis.hash(&mut h);
            r.unit.hash(&mut h);
            r.value.hash(&mut h);
            r.priority.hash(&mut h);
            r.enabled.hash(&mut h);
            r.tolerance_cents.hash(&mut h);
            r.early_warning_pct.hash(&mut h);
            r.params_json.hash(&mut h);
        }
        self.initial_balance.0.hash(&mut h);
        self.leverage.hash(&mut h);
        self.profit_target_pct.0.hash(&mut h);
        h.finalize_hex()
    }

    /// Looks up the rule id for a given kind/name. Useful for cross-referencing
    /// a `RuleEntry` with a registered rule implementation.
    #[must_use]
    pub fn rule_id_for(kind: &str) -> RuleId {
        RuleId::named(kind)
    }

    /// Serializes the pack to a pretty-printed JSON string. Requires the
    /// `serialization` feature.
    #[cfg(feature = "serialization")]
    pub fn to_json(&self) -> crate::Result<String> {
        use serde_json::json;
        let rules_json: Vec<serde_json::Value> = self
            .rules
            .iter()
            .map(|r| {
                json!({
                    "id": r.id,
                    "kind": r.kind,
                    "basis": r.basis.to_string(),
                    "unit": r.unit.to_string(),
                    "value": r.value,
                    "tolerance_cents": r.tolerance_cents,
                    "early_warning_pct": r.early_warning_pct,
                    "priority": r.priority,
                    "enabled": r.enabled,
                    "params": r.params_json,
                })
            })
            .collect();
        let json = json!({
            "id": self.id,
            "version": self.version,
            "tenant_id": self.tenant_id.to_string(),
            "lifecycle": self.lifecycle.to_string(),
            "effective_from": self.effective_from.to_rfc3339(),
            "superseded_by": self.superseded_by,
            "description": self.description,
            "initial_balance": self.initial_balance.0,
            "leverage": self.leverage,
            "profit_target_pct": self.profit_target_pct.0,
            "rules": rules_json,
            "content_hash": self.content_hash(),
        });
        serde_json::to_string_pretty(&json).map_err(|e| crate::Error::Serialization(e.to_string()))
    }
}
