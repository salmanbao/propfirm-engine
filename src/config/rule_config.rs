//! Per-rule configuration. Each rule carries its own config struct so custom
//! overrides can be applied without mutating the global plan.

use crate::core::types::{dec, Pct};

/// Override flags for individual rules.
#[derive(Debug, Clone, Default)]
pub struct RuleConfig {
    pub daily_drawdown: Option<DailyDrawdownConfig>,
    pub max_drawdown: Option<MaxDrawdownConfig>,
    pub trailing_drawdown: Option<TrailingDrawdownConfig>,
    pub profit_target: Option<ProfitTargetConfig>,
    pub min_trading_days: Option<MinTradingDaysConfig>,
    pub consistency: Option<ConsistencyConfig>,
    pub news_trading: Option<NewsTradingConfig>,
    pub overnight_holding: Option<OvernightConfig>,
    pub weekend_holding: Option<WeekendConfig>,
    pub max_position_size: Option<MaxPositionSizeConfig>,
    pub max_open_positions: Option<MaxOpenPositionsConfig>,
    pub max_daily_trades: Option<MaxDailyTradesConfig>,
    pub time_limit: Option<TimeLimitConfig>,
    pub cooldown: Option<CooldownConfig>,
    pub hedging: Option<HedgingConfig>,
    pub grid_trading: Option<GridTradingConfig>,
    pub copy_trading: Option<CopyTradingConfig>,
    pub sl_required: Option<StopLossRequiredConfig>,
    pub tp_required: Option<TakeProfitRequiredConfig>,
}

#[derive(Debug, Clone)]
pub struct DailyDrawdownConfig {
    pub pct: Pct,
    pub on_balance: bool,
}

#[derive(Debug, Clone)]
pub struct MaxDrawdownConfig {
    pub pct: Pct,
    pub on_balance: bool,
}

#[derive(Debug, Clone)]
pub struct TrailingDrawdownConfig {
    pub pct: Pct,
    pub start_at_pct: Pct, // trail starts once profit reaches this % of balance
}

#[derive(Debug, Clone)]
pub struct ProfitTargetConfig {
    pub pct: Pct,
}

#[derive(Debug, Clone)]
pub struct MinTradingDaysConfig {
    pub days: u32,
}

#[derive(Debug, Clone)]
pub struct ConsistencyConfig {
    /// Largest single-day profit cannot exceed this % of total profit.
    pub max_day_profit_share: Pct,
}

#[derive(Debug, Clone)]
pub struct NewsTradingConfig {
    /// Minutes before/after news event during which trading is restricted.
    pub window_minutes: u32,
    /// Whether to allow closing existing positions during news.
    pub allow_closes: bool,
}

#[derive(Debug, Clone)]
pub struct OvernightConfig {
    /// Server hour at which overnight rule activates (positions must be closed by this hour).
    pub forbidden_from_hour: u8,
    pub forbidden_to_hour: u8,
}

#[derive(Debug, Clone)]
pub struct WeekendConfig {
    /// Server hour at which weekend rule activates (typically Friday close).
    pub forbidden_from_hour: u8,
    pub weekend_starts_hour: u8,
}

#[derive(Debug, Clone)]
pub struct MaxPositionSizeConfig {
    pub max_lots_per_order: rust_decimal::Decimal,
}

#[derive(Debug, Clone)]
pub struct MaxOpenPositionsConfig {
    pub max_count: u32,
}

#[derive(Debug, Clone)]
pub struct MaxDailyTradesConfig {
    pub max_count: u32,
}

#[derive(Debug, Clone)]
pub struct TimeLimitConfig {
    pub days: u32,
}

#[derive(Debug, Clone)]
pub struct CooldownConfig {
    pub seconds_between_trades: u64,
}

#[derive(Debug, Clone)]
pub struct HedgingConfig {
    pub allowed: bool,
}

#[derive(Debug, Clone)]
pub struct GridTradingConfig {
    pub allowed: bool,
    pub min_grid_spacing_pips: u32,
}

#[derive(Debug, Clone)]
pub struct CopyTradingConfig {
    pub allowed: bool,
}

#[derive(Debug, Clone)]
pub struct StopLossRequiredConfig {
    pub required: bool,
    pub min_distance_pips: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct TakeProfitRequiredConfig {
    pub required: bool,
}

impl RuleConfig {
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn from_plan(plan: &crate::config::plan::ChallengePlan) -> Self {
        let mut cfg = Self::empty();
        cfg.daily_drawdown = Some(DailyDrawdownConfig {
            pct: plan.max_daily_drawdown_pct,
            on_balance: plan.drawdown_on_balance,
        });
        cfg.max_drawdown = Some(MaxDrawdownConfig {
            pct: plan.max_total_drawdown_pct,
            on_balance: plan.drawdown_on_balance,
        });
        if plan.trailing_drawdown_enabled {
            cfg.trailing_drawdown = Some(TrailingDrawdownConfig {
                pct: plan.trailing_drawdown_pct,
                start_at_pct: dec!(0).into(),
            });
        }
        cfg.profit_target = Some(ProfitTargetConfig {
            pct: plan.profit_target_pct,
        });
        cfg.min_trading_days = Some(MinTradingDaysConfig {
            days: plan.min_trading_days,
        });
        if let Some(c) = plan.consistency_pct {
            cfg.consistency = Some(ConsistencyConfig {
                max_day_profit_share: c,
            });
        }
        cfg.news_trading = Some(NewsTradingConfig {
            window_minutes: 2,
            allow_closes: true,
        });
        cfg.overnight_holding = Some(OvernightConfig {
            forbidden_from_hour: 22,
            forbidden_to_hour: 7,
        });
        cfg.weekend_holding = Some(WeekendConfig {
            forbidden_from_hour: 21,
            weekend_starts_hour: 21,
        });
        if let Some(l) = plan.max_position_lots {
            cfg.max_position_size = Some(MaxPositionSizeConfig {
                max_lots_per_order: l,
            });
        }
        if let Some(c) = plan.max_open_positions {
            cfg.max_open_positions = Some(MaxOpenPositionsConfig { max_count: c });
        }
        if let Some(c) = plan.max_daily_trades {
            cfg.max_daily_trades = Some(MaxDailyTradesConfig { max_count: c });
        }
        if let Some(d) = plan.time_limit_days {
            cfg.time_limit = Some(TimeLimitConfig { days: d });
        }
        if plan.cooldown_seconds > 0 {
            cfg.cooldown = Some(CooldownConfig {
                seconds_between_trades: plan.cooldown_seconds,
            });
        }
        cfg.hedging = Some(HedgingConfig {
            allowed: plan.hedging_allowed,
        });
        cfg.grid_trading = Some(GridTradingConfig {
            allowed: plan.grid_trading_allowed,
            min_grid_spacing_pips: 50,
        });
        cfg.copy_trading = Some(CopyTradingConfig {
            allowed: plan.copy_trading_allowed,
        });
        cfg.sl_required = Some(StopLossRequiredConfig {
            required: plan.require_stop_loss,
            min_distance_pips: None,
        });
        cfg.tp_required = Some(TakeProfitRequiredConfig {
            required: plan.require_take_profit,
        });
        cfg
    }
}
