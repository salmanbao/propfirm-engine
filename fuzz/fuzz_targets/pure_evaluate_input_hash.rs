//! Fuzz target for `pure::evaluate` + `input_hash` determinism.
//!
//! ## What this tests
//!
//! The pure evaluate function produces an `input_hash` (sha256) of
//! all evaluation inputs. The hash must be deterministic — calling
//! `pure::evaluate` twice with the same inputs MUST produce the
//! same hash byte-for-byte. This fuzz target verifies that
//! property across the input space.
//!
//! ## Run locally
//!
//! ```bash
//! cargo +nightly fuzz run pure_evaluate_input_hash -- -max_total_time=60
//! ```

#![no_main]

use libfuzzer_sys::fuzz_target;
use propfirm::core::account::{Account, AccountStatus};
use propfirm::core::ids::AccountId;
use propfirm::core::tick::{Quote, Tick};
use propfirm::core::types::{Money, Price, ServerTime, Symbol};
use propfirm::pure::{self, EquitySource, EvaluateInputs};
use propfirm::rulepack::RulePack;
use propfirm::rules::context::RuleContextKind;
use propfirm::rules::registry::RuleRegistry;
use propfirm::config::presets::ftmo_phase1;
use propfirm::tenant::TenantId;
use rust_decimal_macros::dec;

fuzz_target!(|data: &[u8]| {
    if data.len() < 8 {
        return;
    }
    let equity = i64::from_le_bytes([data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7]]);
    let equity_dec = rust_decimal::Decimal::from(equity.abs());

    let plan = ftmo_phase1();
    let tenant = TenantId::named("fuzz");
    let account_id = AccountId::new();
    let mut acc = Account::new(account_id, plan.clone()).with_tenant(tenant);
    acc.status = AccountStatus::Active;
    acc.balance = Money(equity_dec);
    acc.equity = Money(equity_dec);
    acc.peak_balance = Money(equity_dec);
    acc.peak_equity = Money(equity_dec);
    acc.day_start_balance = Money(equity_dec);
    acc.day_start_equity = Money(equity_dec);

    let pack = RulePack::synthetic_from_plan(account_id, tenant, &plan);
    let registry = RuleRegistry::with_default_rules_for_plan(&plan);

    let now = chrono::Utc::now();
    let quote = Quote { bid: Price(equity_dec), ask: Price(equity_dec), ts: now };
    let tick = Tick::new(Symbol::new("EURUSD"), quote);

    let inputs = EvaluateInputs::for_tick(&[], &[], &tick).with_equity_source(EquitySource::Estimated);

    // Run twice — must produce the same hash.
    let v1 = pure::evaluate(&acc, &pack, &registry, RuleContextKind::OnTick, ServerTime(now), inputs);
    let v2 = pure::evaluate(&acc, &pack, &registry, RuleContextKind::OnTick, ServerTime(now), inputs);

    // Either both Ok with the same hash, or both Err.
    match (v1, v2) {
        (Ok(a), Ok(b)) => {
            if a.input_hash != b.input_hash {
                panic!(
                    "input_hash mismatch for same inputs: {} vs {}",
                    a.input_hash, b.input_hash
                );
            }
        }
        (Err(_), Err(_)) => {} // consistent failure is fine
        _ => panic!("inconsistent: one ok, one err"),
    }
});
