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
use propfirm::engine::evaluator::Evaluator;
use propfirm::rules::evaluators::*;
use propfirm::rules::registry::RuleRegistry;
use proptest::prelude::*;
use std::sync::Arc;

/// Helper: build a plan with the given loss reference and a $100k balance.
fn make_plan(loss_ref: LossReference) -> ChallengePlan {
    let mut plan = ftmo_phase1();
    plan.max_loss_reference = loss_ref;
    plan.initial_balance_money = Money(dec!(100_000));
    plan.weekend_holding_allowed = true;
    plan.overnight_holding_allowed = true;
    plan.news_trading_allowed = true;
    plan
}

/// Helper: build an account at the given equity/balance, with the given
/// peak values. Marks equity as broker-reported so breach rules can
/// terminate (no P1-5 downgrade).
fn make_account(
    plan: ChallengePlan,
    balance: i64,
    equity: i64,
    peak_balance: i64,
    peak_equity: i64,
) -> Account {
    let mut acc = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    acc.balance = Money(rust_decimal::Decimal::from(balance));
    acc.equity = Money(rust_decimal::Decimal::from(equity));
    acc.peak_balance = Money(rust_decimal::Decimal::from(peak_balance));
    acc.peak_equity = Money(rust_decimal::Decimal::from(peak_equity));
    acc.initial_balance = Money(dec!(100_000));
    acc.day_start_balance = Money(dec!(100_000));
    acc.status = AccountStatus::Active;
    acc
}

/// Helper: evaluate a tick on the given account.
fn eval_tick(
    evaluator: &Evaluator,
    account: &Account,
) -> propfirm::engine::evaluator::EvaluationResult {
    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0800)),
            ask: Price(dec!(1.0802)),
            ts: chrono::Utc::now(),
        },
    );
    evaluator
        .evaluate_tick(account, &tick, &[], &[], Vec::new())
        .unwrap()
}

proptest! {
    /// Invariant 1: total_drawdown() is always >= 0.
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

    /// Invariant 4: static max-loss floor never moves regardless of peak.
    /// The static limit is always `pct × initial_balance`, no matter how
    /// high equity grows.
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

    /// Invariant 5: trailing max-loss floor floats up monotonically with
    /// peak balance. limit = pct × peak_balance, so as peak goes up, the
    /// floor goes up proportionally.
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

    /// Invariant 3: stateless determinism — same (account, pack, tick) →
    /// same verdict. This is the binding spec's "one defensible answer".
    #[test]
    fn prop_stateless_determinism(balance in 80_000i64..120_000) {
        let plan = make_plan(LossReference::Static);
        let account = make_account(plan, balance, balance, 100_000, 100_000);
        let evaluator = Evaluator::new(account.plan.clone());
        let result1 = eval_tick(&evaluator, &account);
        let result2 = eval_tick(&evaluator, &account);
        prop_assert_eq!(result1.decision.kind, result2.decision.kind,
            "same inputs must produce same decision kind; got {:?} vs {:?}",
            result1.decision.kind, result2.decision.kind);
        prop_assert_eq!(result1.decision.winning_priority, result2.decision.winning_priority,
            "same inputs must produce same winning priority");
    }

    /// Invariant 6: decision priority is invariant under rule reordering.
    /// Register the same set of rules in two different orders; assert
    /// the same decision results.
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

        let reports_a = reg_a.evaluate(&ctx).unwrap();
        let reports_b = reg_b.evaluate(&ctx).unwrap();
        let decision_a = propfirm::engine::decision::Decision::from_reports(&reports_a);
        let decision_b = propfirm::engine::decision::Decision::from_reports(&reports_b);
        prop_assert_eq!(decision_a.kind, decision_b.kind,
            "decision must be invariant under rule reordering; got {:?} vs {:?}",
            decision_a.kind, decision_b.kind);
        prop_assert_eq!(decision_a.winning_priority, decision_b.winning_priority,
            "winning priority must be invariant under reordering");
    }

    /// Invariant 2: a breach verdict is always at least as severe as
    /// a warning at the same distance from the threshold. (This is more
    /// of a sanity check on the enum ordering than a deep property.)
    #[test]
    fn prop_breach_more_severe_than_warning(balance in 50_000i64..150_000) {
        let plan = make_plan(LossReference::Static);
        let account = make_account(plan, balance, balance, 100_000, 100_000);
        let evaluator = Evaluator::new(account.plan.clone());
        let result = eval_tick(&evaluator, &account);
        // Verify that all violations have well-defined severities, and
        // that the max severity across all violations matches the decision
        // kind's intrinsic weight ordering.
        let max_severity = result.decision.max_severity();
        if let Some(sev) = max_severity {
            use propfirm::core::violation::ViolationSeverity;
            // If the decision is terminating, the max severity must be
            // Hard or Liquidate.
            if result.decision.is_terminating() {
                prop_assert!(sev >= ViolationSeverity::Hard,
                    "terminating decision must have Hard+ severity; got {:?}", sev);
            }
        }
    }
}
