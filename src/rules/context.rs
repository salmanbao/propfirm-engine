//! Rule context: the input bundle passed to every [`Rule`] evaluation.
//!
//! The context provides read-only access to the account, optional pending
//! order / trade / tick that triggered the evaluation, and the engine's
//! evaluation scope (open order, trade fill, tick revaluation, day rollover).

use crate::core::account::Account;
use crate::core::events::DomainEvent;
use crate::core::order::Order;
use crate::core::position::Position;
use crate::core::tick::Tick;
use crate::core::trade::Trade;
use crate::core::types::{Timestamp, ServerTime};
use crate::config::rule_config::RuleConfig;
use crate::equity_input::EquityInput;

/// What triggered the evaluation. Determines which rules apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleContextKind {
    /// A new order is being submitted. Pre-trade validation.
    OnOrderSubmit,
    /// A trade has just been filled. Post-trade state update.
    OnTradeFill,
    /// A new market tick arrived. Equity re-evaluation.
    OnTick,
    /// A new trading day has begun (rollover).
    OnDayRollover,
    /// End-of-day evaluation (close of trading session).
    OnEndOfDay,
    /// Manual evaluation request (e.g. dashboard refresh).
    OnDemand,
}

/// Scope of evaluation. Allows a rule to declare whether it runs pre-trade,
/// post-trade, or on tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EvaluationScope {
    PreTrade,
    PostTrade,
    OnTick,
    Periodic,
    OnDemand,
}

/// The rule context passed into every [`Rule::evaluate`](crate::rules::traits::Rule::evaluate)
/// call. Cheap to clone (Arc-backed).
#[derive(Debug, Clone)]
pub struct RuleContext {
    pub account: Account,
    pub open_positions: Vec<Position>,
    pub today_trades: Vec<Trade>,
    pub pending_order: Option<Order>,
    pub latest_trade: Option<Trade>,
    pub latest_tick: Option<Tick>,
    pub recent_events: Vec<DomainEvent>,
    pub server_time: ServerTime,
    pub kind: RuleContextKind,
    pub rule_config: RuleConfig,
    /// **P1-5 fix**: tagged equity input — broker-reported or estimated.
    /// Breach-capable rules call [`Self::equity_is_broker_reported`]
    /// (or use `equity_input.broker_equity()`) to refuse termination
    /// on an estimate.
    pub equity_input: EquityInput,
}

impl RuleContext {
    pub fn new(account: Account) -> Self {
        RuleContext {
            account,
            open_positions: Vec::new(),
            today_trades: Vec::new(),
            pending_order: None,
            latest_trade: None,
            latest_tick: None,
            recent_events: Vec::new(),
            server_time: ServerTime::now(),
            kind: RuleContextKind::OnDemand,
            rule_config: RuleConfig::empty(),
            equity_input: EquityInput::default(),
        }
    }

    /// **P1-5 fix**: returns true only if the equity input on this context
    /// is broker-reported. Breach-capable rules check this before emitting
    /// `Fail`/`Liquidate` — if false, the rule must downgrade to `Warn`
    /// at most (no termination on an estimate).
    pub fn equity_is_broker_reported(&self) -> bool {
        self.equity_input.is_broker_reported()
    }

    /// **P1-5 fix**: builder-style setter to mark the equity input as
    /// broker-reported. Called by the pipeline when the tick event
    /// carries the broker's own equity number (the only valid source
    /// for breach decisions).
    pub fn with_broker_equity(mut self, equity: crate::core::types::Money, balance: crate::core::types::Money) -> Self {
        self.equity_input = EquityInput::BrokerReported { equity, balance };
        self
    }

    /// **P1-5 fix**: builder-style setter to mark the equity input as
    /// engine-derived (estimated). The default; breach-capable rules will
    /// refuse to terminate on this.
    pub fn with_estimated_equity(mut self, equity: crate::core::types::Money, balance: crate::core::types::Money) -> Self {
        self.equity_input = EquityInput::Estimated { equity, balance };
        self
    }

    pub fn for_open_order(account: Account, order: &Order) -> Self {
        let mut ctx = Self::new(account);
        ctx.pending_order = Some(order.clone());
        ctx.kind = RuleContextKind::OnOrderSubmit;
        ctx.rule_config = RuleConfig::from_plan(&ctx.account.plan);
        ctx
    }

    pub fn for_trade_fill(account: Account, trade: &Trade) -> Self {
        let mut ctx = Self::new(account);
        ctx.latest_trade = Some(trade.clone());
        ctx.kind = RuleContextKind::OnTradeFill;
        ctx.rule_config = RuleConfig::from_plan(&ctx.account.plan);
        ctx
    }

    pub fn for_tick(account: Account, tick: &Tick) -> Self {
        let mut ctx = Self::new(account);
        ctx.latest_tick = Some(tick.clone());
        ctx.kind = RuleContextKind::OnTick;
        ctx.rule_config = RuleConfig::from_plan(&ctx.account.plan);
        ctx
    }

    pub fn for_day_rollover(account: Account) -> Self {
        let mut ctx = Self::new(account);
        ctx.kind = RuleContextKind::OnDayRollover;
        ctx.rule_config = RuleConfig::from_plan(&ctx.account.plan);
        ctx
    }

    pub fn at(server_time: ServerTime) -> Self {
        let mut ctx = Self::new(Account::default_for_tests());
        ctx.server_time = server_time;
        ctx
    }

    /// Returns true if the context kind is `OnOrderSubmit`.
    pub fn is_pre_trade(&self) -> bool {
        self.kind == RuleContextKind::OnOrderSubmit
    }

    /// Returns true if the context kind is `OnTick`.
    pub fn is_on_tick(&self) -> bool {
        self.kind == RuleContextKind::OnTick
    }

    /// Number of currently open positions (across all symbols).
    pub fn open_position_count(&self) -> usize {
        self.open_positions.iter().filter(|p| p.is_open()).count()
    }

    /// Total lots currently open.
    pub fn total_open_lots(&self) -> rust_decimal::Decimal {
        self.open_positions
            .iter()
            .filter(|p| p.is_open())
            .map(|p| p.open_quantity.0)
            .sum()
    }

    /// Today's trade count.
    pub fn today_trade_count(&self) -> usize {
        self.today_trades.len()
    }
}

/// Stub account used as a fallback when contexts are constructed directly
/// via `at()`. Useful for unit tests of individual rules.
trait DefaultForTests {
    fn default_for_tests() -> Self;
}

impl DefaultForTests for Account {
    fn default_for_tests() -> Self {
        Account::new(crate::core::ids::AccountId::new(), crate::config::plan::ChallengePlan::default())
    }
}
