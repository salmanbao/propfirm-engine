#![allow(clippy::missing_const_for_fn)]

use propfirm::config::plan::{ChallengePlan, LossReference};
use propfirm::config::presets::ftmo_phase1;
use propfirm::core::account::Account;
use propfirm::core::ids::AccountId;
use propfirm::core::tick::{Quote, Tick};
use propfirm::core::types::{dec, Money, Price, ServerTime, Symbol};
use propfirm::pure::{evaluate, EvaluateInputs};
use propfirm::rulepack::RulePack;
use propfirm::rules::registry::RuleRegistry;

fn default_plan() -> ChallengePlan {
    let mut plan = ftmo_phase1();
    plan.max_loss_reference = LossReference::Static;
    plan.initial_balance_money = Money(dec!(100_000));
    plan
}

fn default_account() -> Account {
    let plan = default_plan();
    Account::new(AccountId::new(), plan)
}

fn default_pack() -> RulePack {
    RulePack::default_for_plan(&default_plan()).expect("default pack must build")
}

fn default_registry() -> RuleRegistry {
    RuleRegistry::build_from_pack(&default_pack()).expect("default registry must build")
}

fn default_tick() -> Tick {
    Tick::new(
        Symbol::new("EURUSD"),
        Quote {
            bid: Price(dec!(1.0000)),
            ask: Price(dec!(1.0001)),
            ts: chrono::Utc::now(),
        },
    )
}

fn default_inputs() -> EvaluateInputs<'_> {
    EvaluateInputs::for_tick(&[], &[], &default_tick())
        .with_equity_source(propfirm::pure::EquitySource::BrokerReported {
            equity: Money(dec!(100_000)),
            balance: Money(dec!(100_000)),
        })
}

fn fuzz_target(data: &[u8]) {
    if data.len() < 4 {
        return;
    }
    let account = default_account();
    let pack = default_pack();
    let registry = default_registry();
    let tick = default_tick();
    let server_time = ServerTime::now();
    let inputs = default_inputs();

    let _ = evaluate(&account, &pack, &registry, propfirm::rules::context::RuleContextKind::OnTick, server_time, inputs);
}

fuzz_target::run!(fuzz_evaluate = fuzz_target);
