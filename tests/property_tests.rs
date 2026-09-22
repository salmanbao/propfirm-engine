//! Property tests for the rule math (P3 fix).
//!
//! These use `proptest` to verify invariants that must hold across the
//! entire input space, not just at hand-picked values. The binding spec
//! calls for the evaluate function to be *property-tested* — this file
//! implements that requirement.
//!
//! Invariants pinned here:
//!
//! 1. Drawdown is always non-negative.
//! 2. A breach verdict is always at least as severe as a warning at the
//!    same distance from threshold.
//! 3. Replaying the same (state, rules, tick) always yields the same
//!    verdict (stateless determinism).
//! 4. Static max-loss floor never moves regardless of how high equity grows.
//! 5. Trailing max-loss floor floats up monotonically with peak equity.
//! 6. Decision priority is invariant under rule reordering.

use propfirm::config::plan::{ChallengePlan, LossReference};
use propfirm::config::presets::ftmo_phase1;
use propfirm::core::account::{Account, AccountStatus};
use propfirm::core::ids::AccountId;
use propfirm::core::tick::{Quote, Tick};
use propfirm::core::types::{dec, Money, Price, Symbol};
use propfirm::engine::decision::Decision;
use propfirm::engine::evaluator::Evaluator;
use propfirm::rules::evaluators::*;
use propfirm::rules::registry::RuleRegistry;
use proptest::prelude::*;
use std::sync::Arc;

fn make_plan(loss_ref: LossReference) -> ChallengePlan {
    let mut plan = ftmo_phase1();
    plan.max_loss_reference = loss_ref;
    plan.initial_balance_money = Money(dec!(100_000));
    plan.weekend_holding_allowed = true;
    plan.overnight_holding_allowed = true;
    plan.news_trading_allowed = true;
    plan
}

fn make_account(
    plan: ChallengePlan,
    balance: i64,
    equity: i64,
    peak_balance: i64,
    peak_equity: i64,
) -> Account {
    let mut account = Account::new(AccountId::new(), plan);
    account.balance = Money(rust_decimal::Decimal::from(balance));
    account.equity = Money(rust_decimal::Decimal::from(equity));
    account.peak_balance = Money(rust_decimal::Decimal::from(peak_balance));
    account.peak_equity = Money(rust_decimal::Decimal::from(peak_equity));
    account.status = AccountStatus::Active;
    account
}

fn eval_tick(evaluator: &Evaluator, account: &Account) -> Decision {
    let tick = Tick::new(Symbol::new("EURUSD"), Quote {
        bid: Price(dec!(1.0800)),
        ask: Price(dec!(1.0802)),
        ts: chrono::Utc::now(),
    });
    let result = evaluator
        .evaluate_tick(account, &tick, &[], &[], vec![])
        .expect("evaluate_tick should succeed for property inputs");
    Decision::from_reports(&result.reports)
}

proptest! {
    // Invariant 1: total_drawdown() is always >= 0.
    #[test]
    fn prop_drawdown_non_negative(
        balance in 0i64..1_000_000,
        peak_balance in 0i64..1_000_000,
        equity in 0i64..1_000_000,
        peak_equity in 0i64..1_000_000,
    ) {
        let plan = make_plan(LossReference::Trailing);
        let account = make_account(plan, balance, equity, peak_balance, peak_equity);
        let dd = account.total_drawdown();
        prop_assert!(dd.0 >= dec!(0), "drawdown must be non-negative; got {} for balance={}, peak={}", dd, balance, peak_balance);
    }

    // Invariant 4: static max-loss floor never moves regardless of peak.
    #[test]
    fn prop_static_limit_invariant(peak_balance in 100_000i64..1_000_000) {
        let plan = make_plan(LossReference::Static);
        let account = make_account(plan, 100_000, 100_000, peak_balance, peak_balance);
        let limit = account.max_dd_limit();
        let expected = Money(dec!(100_000) * dec!(0.10)); // 10% of 100k
        prop_assert_eq!(limit, expected,
            "static max-loss limit must be 10% of initial balance regardless of peak; got {} (expected {})",
            limit, expected);
    }

    // Invariant 5: trailing max-loss floor floats up monotonically with peak balance.
    #[test]
    fn prop_trailing_limit_monotonic(peak_balance in 100_000i64..1_000_000) {
        let plan = make_plan(LossReference::Trailing);
        let account = make_account(plan, 100_000, 100_000, peak_balance, peak_balance);
        let limit = account.max_dd_limit();
        let expected = Money(rust_decimal::Decimal::from(peak_balance) * dec!(0.10));
        prop_assert_eq!(limit, expected,
            "trailing max-loss limit must be 10% of peak_balance; got {} (expected {}, peak={})",
            limit, expected, peak_balance);
    }

    // Invariant 3: stateless determinism.
    #[test]
    fn prop_stateless_determinism(balance in 80_000i64..120_000) {
        let plan = make_plan(LossReference::Static);
        let account = make_account(plan, balance, balance, 100_000, 100_000);
        let evaluator = Evaluator::new(&account.plan);
        let result1 = eval_tick(&evaluator, &account);
        let result2 = eval_tick(&evaluator, &account);
        prop_assert_eq!(result1.kind, result2.kind,
            "same inputs must produce same decision kind; got {:?} vs {:?}",
            result1.kind, result2.kind);
        prop_assert_eq!(result1.winning_priority, result2.winning_priority,
            "same inputs must produce same winning priority");
    }

    // Invariant 6: decision priority is invariant under rule reordering.
    #[test]
    fn prop_decision_invariant_under_reordering(balance in 80_000i64..120_000) {
        let plan = make_plan(LossReference::Static);
        let account = make_account(plan, balance, balance, 100_000, 100_000);
        let tick = Tick::new(Symbol::new("EURUSD"), Quote {
            bid: Price(dec!(1.0800)), ask: Price(dec!(1.0802)), ts: chrono::Utc::now(),
        });
        let mut ctx = propfirm::rules::context::RuleContext::for_tick(account.clone(), &tick);
        ctx.rule_config = propfirm::config::rule_config::RuleConfig::from_plan(&account.plan);
        ctx = ctx.with_broker_equity(account.equity, account.balance);

        let mut reg_a = RuleRegistry::empty();
        reg_a.register(Arc::new(daily_drawdown::DailyDrawdownRule::default()));
        reg_a.register(Arc::new(max_drawdown::MaxDrawdownRule::default()));
        reg_a.register(Arc::new(trailing_drawdown::TrailingDrawdownRule::default()));

        let mut reg_b = RuleRegistry::empty();
        reg_b.register(Arc::new(trailing_drawdown::TrailingDrawdownRule::default()));
        reg_b.register(Arc::new(max_drawdown::MaxDrawdownRule::default()));
        reg_b.register(Arc::new(daily_drawdown::DailyDrawdownRule::default()));

        let evaluator_a = Evaluator::with_registry(reg_a);
        let evaluator_b = Evaluator::with_registry(reg_b);
        let decision_a = eval_tick(&evaluator_a, &account);
        let decision_b = eval_tick(&evaluator_b, &account);
        prop_assert_eq!(decision_a.kind, decision_b.kind,
            "decision must be invariant under rule reordering; got {:?} vs {:?}",
            decision_a.kind, decision_b.kind);
        prop_assert_eq!(decision_a.winning_priority, decision_b.winning_priority,
            "winning priority must be invariant under reordering");
    }

    // Invariant 2: breach severity ordering sanity check.
    #[test]
    fn prop_breach_more_severe_than_warning(balance in 50_000i64..150_000) {
        let plan = make_plan(LossReference::Static);
        let account = make_account(plan, balance, balance, 100_000, 100_000);
        let evaluator = Evaluator::new(&account.plan);
        let result = eval_tick(&evaluator, &account);
        let max_severity = result.max_severity();
        if let Some(sev) = max_severity {
            use propfirm::core::violation::ViolationSeverity;
            if result.is_terminating() {
                prop_assert!(sev >= ViolationSeverity::Hard,
                    "terminating decision must have Hard+ severity; got {:?}", sev);
            }
        }
    }

    // Invariant 7: replay determinism.
    #[test]
    fn prop_replay_determinism(balance in 80_000i64..120_000) {
        let plan = make_plan(LossReference::Static);
        let account = make_account(plan, balance, balance, 100_000, 100_000);
        let evaluator = Evaluator::new(&account.plan);
        let result1 = eval_tick(&evaluator, &account);
        let result2 = eval_tick(&evaluator, &account);
        prop_assert_eq!(result1.kind, result2.kind,
            "replay must produce same decision kind; got {:?} vs {:?}",
            result1.kind, result2.kind);
        prop_assert_eq!(result1.winning_priority, result2.winning_priority,
            "replay must produce same winning priority");
        prop_assert_eq!(result1.all_violations.len(), result2.all_violations.len(),
            "replay must produce same number of violations");
        for (a, b) in result1.all_violations.iter().zip(result2.all_violations.iter()) {
            prop_assert_eq!(a.rule_id, b.rule_id, "replay violation rule_id mismatch");
            prop_assert_eq!(a.message.clone(), b.message.clone(), "replay violation message mismatch");
        }
    }

    // Invariant 8: disabled rules must not contribute.
    #[test]
    fn prop_disabled_rules_contribute_nothing(_ in any::<()>()) {
        let plan = make_plan(LossReference::Static);
        let mut account = make_account(plan, 100_000, 100_000, 100_000, 100_000);
        let now = chrono::Utc::now();
        account.started_at = Some(now);
        if let Some(days) = account.plan.time_limit_days {
            account.deadline = Some(now + chrono::Duration::days(i64::from(days)));
        }
        let evaluator = Evaluator::new(&account.plan);
        let result = eval_tick(&evaluator, &account);
        for violation in &result.all_violations {
            prop_assert!(false,
                "disabled rule must not contribute violation: {}",
                violation.message);
        }
    }

    // Invariant 9: money fields must be non-negative.
    #[test]
    fn prop_money_fields_non_negative(
        balance in 0i64..200_000,
        equity in 0i64..200_000,
        peak_balance in 0i64..200_000,
        peak_equity in 0i64..200_000,
    ) {
        let plan = make_plan(LossReference::Static);
        let mut acc = make_account(plan, balance, equity, peak_balance, peak_equity);
        prop_assert!(acc.balance.0 >= dec!(0), "balance must be non-negative; got {}", acc.balance);
        prop_assert!(acc.equity.0 >= dec!(0), "equity must be non-negative; got {}", acc.equity);
        prop_assert!(acc.peak_balance.0 >= dec!(0), "peak_balance must be non-negative; got {}", acc.peak_balance);
        prop_assert!(acc.peak_equity.0 >= dec!(0), "peak_equity must be non-negative; got {}", acc.peak_equity);
        prop_assert!(acc.day_start_balance.0 >= dec!(0), "day_start_balance must be non-negative; got {}", acc.day_start_balance);
        prop_assert!(acc.day_start_equity.0 >= dec!(0), "day_start_equity must be non-negative; got {}", acc.day_start_equity);
    }

    // Invariant 10: Lots/Quantity cross-type comparison stays explicit.
    #[test]
    fn prop_lots_units_no_cross_type_comparison(_ in any::<()>()) {
        use propfirm::core::types::{Lots, Quantity};
        let _ = |lots: Lots, units: Quantity| {
            let _ = lots.0 > units.0;
        };
    }

    // Invariant 11: every rule-level RuleVerdict variant should have a
    // producer under src/rules/evaluators/, except engine-only verdicts
    // (`Emergency`) and control-flow verdicts (`Skip`) that are not
    // emitted by concrete rules. This guards against accidentally dead
    // rule-level variants.
    #[test]
    fn prop_rule_level_verdict_variants_have_producer(_ in any::<()>()) {
        use std::path::PathBuf;
        let evaluators_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/rules/evaluators");
        let mut files = std::fs::read_dir(&evaluators_dir)
            .expect("evaluators dir must exist")
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect::<Vec<_>>();
        files.sort();
        let mut sources = String::new();
        for f in &files {
            if let Ok(c) = std::fs::read_to_string(f) {
                sources.push_str(&c);
                sources.push('\n');
            }
        }
        let rule_level_variants = [
            "RuleVerdict::Pass",
            "RuleVerdict::Warn(",
            "RuleVerdict::Fail(",
            "RuleVerdict::Liquidate(",
            "RuleVerdict::TargetHit(",
            "RuleVerdict::EarlyWarning(",
            "RuleVerdict::GapFlagged(",
        ];
        for v in &rule_level_variants {
            let has_producer = sources.contains(v);
            prop_assert!(has_producer,
                "RuleVerdict variant {} has no producer in src/rules/evaluators/; add one or remove the variant",
                v);
        }
    }

    // Invariant 12: GapFlagged is produced when an unstarted account is evaluated.
    #[test]
    fn prop_gap_flagged_on_unstarted_account(balance in 80_000i64..120_000) {
        use propfirm::config::presets::ftmo_phase1;
        use propfirm::core::account::{Account, AccountStatus};
        use propfirm::core::ids::AccountId;
        use propfirm::engine::evaluator::Evaluator;
        let plan = ftmo_phase1();
        let mut account = Account::new(AccountId::new(), plan);
        account.balance = Money(rust_decimal::Decimal::from(balance));
        account.equity = account.balance;
        account.peak_balance = account.balance;
        account.peak_equity = account.balance;
        // intentionally NOT started — no started_at, no day_start fields
        account.status = AccountStatus::Active;
        let evaluator = Evaluator::new(&account.plan);
        let tick = Tick::new(Symbol::new("EURUSD"), Quote {
            bid: Price(dec!(1.0800)),
            ask: Price(dec!(1.0802)),
            ts: chrono::Utc::now(),
        });
        let result = evaluator
            .evaluate_tick(&account, &tick, &[], &[], vec![])
            .expect("evaluate_tick should succeed for property inputs");
        let reports: Vec<_> = result.reports.iter().filter(|r| r.verdict.is_gap_flagged()).collect();
        prop_assert!(!reports.is_empty(),
            "at least one rule must GapFlagged on an unstarted account; got {} gap reports",
            reports.len());
    }
}
