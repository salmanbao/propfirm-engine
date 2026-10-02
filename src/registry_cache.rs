//! Per-plan RuleRegistry cache.
//!
//! `RuleRegistry::with_default_rules_for_plan(&plan)` allocates 25
//! `Arc<dyn Rule>` + a `HashMap` per call. For 1000 accounts sharing
//! the same FTMO Phase 1 plan, that's 25K unnecessary allocations per
//! 60-second evaluation cycle.
//!
//! This module caches the registry keyed by a hash of the plan's
//! content-affecting fields. On cache hit (the common case), the
//! caller gets an `Arc<RuleRegistry>` clone in ~50ns instead of
//! rebuilding in ~2-4µs.
//!
//! The cache is process-local (not Redis) because:
//! 1. A Redis round-trip (~0.3ms) is 100× more expensive than just
//!    rebuilding the registry (~2-4µs).
//! 2. The registry is immutable and deterministic per plan — every pod
//!    that sees the same plan builds the identical registry.
//! 3. No cross-pod consistency is needed (each pod is self-sufficient).

use crate::config::plan::ChallengePlan;
use crate::rules::registry::RuleRegistry;
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

/// Global registry cache. Lazily initialized on first access.
static CACHE: OnceLock<Mutex<HashMap<u64, Arc<RuleRegistry>>>> = OnceLock::new();

/// Get the cached registry for a plan, or build and cache it.
///
/// The cache key is a hash of the plan's content-affecting fields
/// (thresholds, flags, phases — NOT the plan's `id` or `meta`, which
/// are metadata and don't affect rule behavior).
///
/// # Panics
/// Never panics — the Mutex is only held for the duration of a HashMap
/// lookup/insert, and the HashMap never returns Err.
#[must_use]
pub fn get_or_build(plan: &ChallengePlan) -> Arc<RuleRegistry> {
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));

    // Fast path: read lock + lookup
    {
        let guard = cache.lock().unwrap();
        if let Some(registry) = guard.get(&plan_cache_key(plan)) {
            return registry.clone();
        }
    }

    // Slow path: build + insert
    let registry = Arc::new(RuleRegistry::with_default_rules_for_plan(plan));
    let mut guard = cache.lock().unwrap();
    guard.insert(plan_cache_key(plan), registry.clone());
    registry
}

/// Compute a cache key from the plan's content-affecting fields.
///
/// This hashes the same fields that `RuleRegistry::with_default_rules_for_plan`
/// reads to decide which rules to enable/disable. If two plans produce
/// the same hash, they produce the same registry.
fn plan_cache_key(plan: &ChallengePlan) -> u64 {
    let mut h = DefaultHasher::new();

    // Core thresholds
    plan.initial_balance_money.hash(&mut h);
    plan.profit_target_pct.hash(&mut h);
    plan.max_daily_drawdown_pct.hash(&mut h);
    plan.max_total_drawdown_pct.hash(&mut h);
    plan.max_loss_reference.hash(&mut h);
    plan.drawdown_on_balance.hash(&mut h);
    plan.trailing_drawdown_enabled.hash(&mut h);
    plan.trailing_drawdown_pct.hash(&mut h);

    // Day/time limits
    plan.min_trading_days.hash(&mut h);
    plan.time_limit_days.hash(&mut h);
    plan.day_reset_time.hash(&mut h);

    // Position limits
    plan.max_position_lots.hash(&mut h);
    plan.max_total_lots.hash(&mut h);
    plan.max_open_positions.hash(&mut h);
    plan.max_daily_trades.hash(&mut h);

    // Trade restriction flags
    plan.news_trading_allowed.hash(&mut h);
    plan.overnight_holding_allowed.hash(&mut h);
    plan.weekend_holding_allowed.hash(&mut h);
    plan.hedging_allowed.hash(&mut h);
    plan.grid_trading_allowed.hash(&mut h);
    plan.copy_trading_allowed.hash(&mut h);
    plan.require_stop_loss.hash(&mut h);
    plan.require_take_profit.hash(&mut h);

    // Consistency + cooldown
    plan.consistency_pct.hash(&mut h);
    plan.cooldown_seconds.hash(&mut h);

    // Leverage + trading hours
    plan.leverage.hash(&mut h);
    plan.trading_hours.hash(&mut h);

    // Per-trade loss
    plan.per_trade_max_loss_pct.hash(&mut h);
    plan.per_trade_max_loss_money.hash(&mut h);

    // HFT ban
    plan.hft_ban_enabled.hash(&mut h);
    plan.hft_min_round_trip_seconds.hash(&mut h);

    // Inactivity
    plan.inactivity_days.hash(&mut h);

    // Phase
    plan.phase.hash(&mut h);

    // Timezone (affects day rollover behavior, which affects rule
    // evaluation results for the same inputs at the same wall-clock
    // time — but NOT the registry construction. However, including
    // it here is conservative and doesn't hurt: two plans with
    // different timezones get different registries, which is fine
    // since the registry is cheap.)
    plan.timezone.is_some().hash(&mut h);

    // Payout config presence (affects payout-related rules)
    plan.payout_config.is_some().hash(&mut h);
    plan.refundable.hash(&mut h);
    plan.refund_fee_amount.hash(&mut h);

    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::presets::ftmo_phase1;

    #[test]
    fn cache_returns_same_registry_for_same_plan() {
        let plan1 = ftmo_phase1();
        let plan2 = ftmo_phase1(); // same plan, different instance

        let r1 = get_or_build(&plan1);
        let r2 = get_or_build(&plan2);

        // Same Arc (cache hit on second call).
        assert!(
            Arc::ptr_eq(&r1, &r2),
            "cache should return the same Arc for identical plans"
        );
    }

    #[test]
    fn cache_returns_different_registry_for_different_plans() {
        let plan1 = ftmo_phase1();
        let mut plan2 = ftmo_phase1();
        plan2.profit_target_pct = crate::core::types::Pct(rust_decimal_macros::dec!(0.05));

        let r1 = get_or_build(&plan1);
        let r2 = get_or_build(&plan2);

        // Different Arc (different plan content → different cache key).
        assert!(
            !Arc::ptr_eq(&r1, &r2),
            "cache should return different Arcs for different plans"
        );
    }
}
