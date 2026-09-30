//! Fuzz target for the `RuleRegistry::evaluate` panic-safety path.
//!
//! ## What this tests
//!
//! The registry wraps each rule evaluation in `std::panic::catch_unwind`
//! so a buggy rule degrades to `Warn` instead of crashing the process.
//! This fuzz target verifies that property holds for arbitrary inputs:
//! no input — no matter how malformed — should cause the registry to
//! panic.
//!
//! ## Run locally
//!
//! ```bash
//! cargo +nightly fuzz run rule_registry_panic_safety -- -max_total_time=60
//! ```
//!
//! ## CI
//!
//! CI runs this for 60s (`-max_total_time=60`) on every nightly cron.
//! Fuzz findings are committed to `fuzz/corpus/rule_registry_panic_safety/`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use propfirm::core::account::{Account, AccountStatus, AccountType};
use propfirm::core::ids::AccountId;
use propfirm::core::tick::{Quote, Tick};
use propfirm::core::types::{Money, Pct, Price, ServerTime, Symbol};
use propfirm::rules::context::{RuleContext, RuleContextKind};
use propfirm::rules::registry::RuleRegistry;
use propfirm::config::presets::ftmo_phase1;
use propfirm::tenant::TenantId;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::str::FromStr;

fuzz_target!(|data: &[u8]| {
    // Use the fuzz data to seed account financials — every byte
    // controls one of: balance, equity, peak_balance, day_start, etc.
    if data.len() < 16 {
        return;
    }
    let balance = Decimal::from(data[0] as i64);
    let equity = Decimal::from(data[1] as i64);
    let peak = Decimal::from(data[2] as i64);
    let day_start = Decimal::from(data[3] as i64);
    let today_pnl = Decimal::from_signed_bytes_int([data[4], data[5]]);
    let total_pnl = Decimal::from_signed_bytes_int([data[6], data[7]]);

    let plan = ftmo_phase1();
    let tenant = TenantId::named("fuzz");
    let account_id = AccountId::new();
    let mut acc = Account::new(account_id, plan.clone()).with_tenant(tenant).start(chrono::Utc::now()).unwrap_or_else(|_| {
        // If start fails for some reason, use a pre-built account.
        let mut a = Account::new(account_id, plan.clone()).with_tenant(tenant);
        a.status = AccountStatus::Active;
        a
    });

    acc.balance = Money(balance.max(dec!(0)));
    acc.equity = Money(equity.max(dec!(0)));
    acc.peak_balance = Money(peak.max(dec!(0)));
    acc.peak_equity = Money(peak.max(dec!(0)));
    acc.day_start_balance = Money(day_start.max(dec!(0)));
    acc.day_start_equity = Money(day_start.max(dec!(0)));
    acc.today_realized_pnl = Money(today_pnl);
    acc.total_realized_pnl = Money(total_pnl);

    let registry = RuleRegistry::with_default_rules_for_plan(&plan);
    let now = chrono::Utc::now();
    let quote = Quote { bid: Price(equity), ask: Price(equity), ts: now };
    let tick = Tick::new(Symbol::new("EURUSD"), quote);
    let ctx = RuleContext::for_tick(&acc, &tick)
        .with_server_time(ServerTime(now))
        .with_kind(RuleContextKind::OnTick);

    // This call must NEVER panic — the registry's catch_unwind
    // wraps each rule. If it panics, libfuzzer reports a finding.
    let result = registry.evaluate(&ctx);
    // We don't assert on the result — any verdict (Pass, Warn,
    // Fail, Liquidate, Emergency) is acceptable as long as we got
    // a verdict without panicking.
    let _ = result;
});
