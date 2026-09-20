//! P0-G: property test that proves rule packs are the real source of
//! truth (P0-D fix).
//!
//! Before P0-D, `RuleRegistry::build_from_pack` instantiated each rule
//! with `Default` and never applied the pack entry's `value`/`basis`/etc.
//! — a tenant editing thresholds through the form changed nothing.
//!
//! After P0-D, the factory accepts the `RuleEntry` and constructs a
//! parameterized rule that reads from `self.params` in `evaluate`. This
//! test proves the wiring works: the same account produces a different
//! verdict when the pack entry's `value` is changed.

use propfirm::config::plan::LossReference;
use propfirm::config::presets::ftmo_phase1;
use propfirm::core::account::{Account, AccountStatus};
use propfirm::core::ids::AccountId;
use propfirm::core::tick::{Quote, Tick};
use propfirm::core::types::ServerTime;
use propfirm::core::types::{dec, Money, Price, Symbol};
use propfirm::rulepack::{PackLifecycle, RuleBasis, RuleEntry, RulePack, RuleUnit};
use propfirm::rules::context::RuleContextKind;
use propfirm::rules::registry::RuleRegistry;

/// Helper: build an account at the given equity/peak with a 10% static
/// max-loss preset (so the max_drawdown rule is the only thing in play).
fn make_account() -> Account {
    let plan = ftmo_phase1().with_loss_reference(LossReference::Static);
    let mut acc = Account::new(AccountId::new(), plan)
        .start(chrono::Utc::now())
        .unwrap();
    acc.initial_balance = Money(dec!(100_000));
    // Account at 95k equity — comfortably above the 90k static floor
    // for a 10% max-loss limit. NO breach at 10%.
    acc.balance = Money(dec!(95_000));
    acc.equity = Money(dec!(95_000));
    acc.peak_balance = Money(dec!(100_000));
    acc.peak_equity = Money(dec!(100_000));
    acc.day_start_balance = Money(dec!(95_000)); // suppress daily_dd
    acc.status = AccountStatus::Active;
    acc
}

/// Helper: build a rule pack with one max_drawdown entry at the given pct.
fn make_pack(max_drawdown_pct: rust_decimal::Decimal) -> RulePack {
    RulePack {
        id: "test-pack".into(),
        version: 1,
        tenant_id: propfirm::tenant::TenantId::named("test"),
        lifecycle: PackLifecycle::Active,
        effective_from: chrono::Utc::now(),
        superseded_by: None,
        description: "test pack".into(),
        rules: vec![RuleEntry {
            id: "max_total_loss".into(),
            kind: "max_drawdown".into(),
            basis: RuleBasis::Static,
            unit: RuleUnit::Percent,
            value: max_drawdown_pct,
            tolerance_cents: Some(1),
            early_warning_pct: Some(dec!(0.8)),
            priority: 1000,
            enabled: true,
            params_json: "{}".into(),
        }],
        initial_balance: Money(dec!(100_000)),
        leverage: 100,
        profit_target_pct: dec!(0.10).into(),
    }
}

/// Helper: evaluate the account against a pack and return the decision kind.
fn eval_against_pack(
    account: &Account,
    pack: &RulePack,
) -> propfirm::engine::decision::DecisionKind {
    let registry = RuleRegistry::build_from_pack(pack).unwrap();
    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0800)),
            ask: Price(dec!(1.0802)),
            ts: chrono::Utc::now(),
        },
    );
    let verdict = propfirm::pure::evaluate(
        account,
        pack,
        &registry,
        RuleContextKind::OnTick,
        ServerTime::now(),
        propfirm::pure::EvaluateInputs::for_tick(&[], &[], &tick),
    )
    .unwrap();
    verdict.decision.kind
}

#[test]
fn p0_d_tenant_edits_pack_value_verdict_changes() {
    // Same account. Two packs. Only difference: the max_drawdown entry's
    // `value` field is 0.10 (10%) in pack A, 0.04 (4%) in pack B.
    //
    // At 95k equity on a 100k initial:
    // - 10% static limit → floor = 90k → 95k > 90k → NO breach.
    // - 4% static limit → floor = 96k → 95k < 96k → BREACH.
    //
    // Before P0-D, both packs would produce the same verdict (NO breach)
    // because the rule ignored the entry's `value` and read
    // `ctx.account.plan.max_total_drawdown_pct` instead. After P0-D, the
    // rule reads `self.params.value`, so the verdict actually changes.
    let account = make_account();

    let pack_a = make_pack(dec!(0.10)); // 10% — no breach at 95k
    let pack_b = make_pack(dec!(0.04)); // 4% — breach at 95k

    let decision_a = eval_against_pack(&account, &pack_a);
    let decision_b = eval_against_pack(&account, &pack_b);

    // Pack A (10% limit, account at 95k) → NO breach.
    assert!(
        !decision_a.is_terminating(),
        "pack A (10% limit) should NOT breach at 95k equity; got {:?}",
        decision_a
    );

    // Pack B (4% limit, account at 95k) → BREACH (Liquidate).
    assert!(
        decision_b.is_terminating(),
        "pack B (4% limit) SHOULD breach at 95k equity (95k < 96k floor); got {:?}",
        decision_b
    );

    // The decisions must differ — proving the pack's `value` field is
    // the source of truth, not the plan.
    assert_ne!(
        decision_a, decision_b,
        "P0-D: tenant editing the pack's value field MUST change the verdict"
    );
}

#[test]
fn p0_d_pack_basis_overrides_plan_basis() {
    // Same account, same `value: 0.10`. Pack A uses Static basis; pack B
    // uses Trailing basis. At 95k equity with peak 100k:
    // - Static → limit = 0.10 × 100k = 10k → dd = 5k < 10k → no breach.
    // - Trailing → limit = 0.10 × 100k = 10k → dd = 5k < 10k → no breach.
    //
    // Wait, those are the same — because peak == initial here. Let me
    // make peak higher: peak=200k, equity=185k.
    // - Static → floor = 100k - 10k = 90k → 185k > 90k → no breach.
    // - Trailing → floor = 200k - 20k = 180k → 185k > 180k → no breach (just barely).
    // Let me push to 175k equity:
    // - Static → 175k > 90k → no breach.
    // - Trailing → 175k < 180k → BREACH.
    let mut acc = make_account();
    acc.balance = Money(dec!(175_000));
    acc.equity = Money(dec!(175_000));
    acc.peak_balance = Money(dec!(200_000));
    acc.peak_equity = Money(dec!(200_000));
    acc.day_start_balance = Money(dec!(175_000));

    let mut pack_static = make_pack(dec!(0.10));
    pack_static.rules[0].basis = RuleBasis::Static;
    let mut pack_trailing = make_pack(dec!(0.10));
    pack_trailing.rules[0].basis = RuleBasis::Trailing;

    let decision_static = eval_against_pack(&acc, &pack_static);
    let decision_trailing = eval_against_pack(&acc, &pack_trailing);

    assert!(!decision_static.is_terminating(),
        "static basis (peak 200k, equity 175k, 10% of initial 100k = 10k limit, dd = 25k from initial) — wait, dd should be measured from initial here, dd = 100k - 175k = negative → 0. NO breach. Got {:?}", decision_static);
    assert!(decision_trailing.is_terminating(),
        "trailing basis (peak 200k, equity 175k, 10% of peak 200k = 20k limit, dd = 200k - 175k = 25k > 20k) → BREACH. Got {:?}", decision_trailing);
}

#[test]
fn p0_d_pack_priority_overrides_default() {
    // The pack entry's `priority` field should override the rule's
    // default priority. Build a pack with max_drawdown at priority 42
    // (instead of the default 1000) and verify the winning_priority in
    // the verdict is 42.
    let account = make_account();
    let mut pack = make_pack(dec!(0.04)); // forces a breach
    pack.rules[0].priority = 42;

    let registry = RuleRegistry::build_from_pack(&pack).unwrap();
    let tick = Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0800)),
            ask: Price(dec!(1.0802)),
            ts: chrono::Utc::now(),
        },
    );
    let verdict = propfirm::pure::evaluate(
        &account,
        &pack,
        &registry,
        RuleContextKind::OnTick,
        ServerTime::now(),
        propfirm::pure::EvaluateInputs::for_tick(&[], &[], &tick),
    )
    .unwrap();
    assert!(verdict.decision.is_terminating(), "expected breach");
    assert_eq!(
        verdict.decision.winning_priority, 42,
        "P0-D: pack entry's priority (42) should override the rule's default (1000)"
    );
}

#[test]
fn p0_d_pack_tolerance_overrides_default() {
    // The pack entry's `tolerance_cents` should override the rule's
    // default (1¢). Build a pack with tolerance 100¢ ($1) and verify
    // a near-boundary breach is suppressed.
    let mut account = make_account();
    // Set equity just below the floor: 4% of 100k = 4k limit, floor = 96k.
    // Equity at 95_999.50 → 0.50 below floor.
    // With tolerance 1¢: dd > limit + 0.01 → 0.50 > 0.01 → BREACH.
    // With tolerance 100¢ ($1): dd > limit + 1.00 → 0.50 > 1.00 → NO breach.
    account.balance = Money(dec!(95_999.50));
    account.equity = Money(dec!(95_999.50));
    account.day_start_balance = Money(dec!(95_999.50));

    let mut pack_small_tol = make_pack(dec!(0.04));
    pack_small_tol.rules[0].tolerance_cents = Some(1); // 1¢
    let mut pack_big_tol = make_pack(dec!(0.04));
    pack_big_tol.rules[0].tolerance_cents = Some(100); // $1

    let decision_small = eval_against_pack(&account, &pack_small_tol);
    let decision_big = eval_against_pack(&account, &pack_big_tol);

    assert!(
        decision_small.is_terminating(),
        "with 1¢ tolerance, 0.50 below floor should breach; got {:?}",
        decision_small
    );
    assert!(
        !decision_big.is_terminating(),
        "with $1 tolerance, 0.50 below floor should NOT breach; got {:?}",
        decision_big
    );
}
