//! Risk metrics.
//!
//! All metrics accept a slice of equity samples (point-in-time equity
//! values) and return a single scalar. They use `Decimal` for precision.

use crate::core::types::{Decimal, Money, dec};
use crate::core::Error;
use rust_decimal::MathematicalOps;

/// Aggregated risk metrics.
#[derive(Debug, Clone, Default)]
pub struct RiskMetrics {
    pub sharpe: Decimal,
    pub sortino: Decimal,
    pub calmar: Decimal,
    pub profit_factor: Decimal,
    pub max_drawdown: Decimal,
    pub max_drawdown_pct: Decimal,
    pub expectancy: Decimal,
    pub win_rate: Decimal,
    pub total_trades: usize,
    pub winning_trades: usize,
    pub losing_trades: usize,
    pub avg_win: Decimal,
    pub avg_loss: Decimal,
    pub largest_win: Decimal,
    pub largest_loss: Decimal,
    pub avg_holding_time_minutes: Decimal,
    pub z_score: Decimal,
    pub recovery_factor: Decimal,
}

impl RiskMetrics {
    /// Computes a full risk profile from equity samples and per-trade PnL.
    pub fn compute(equity_curve: &[Money], trades_pnl: &[Money]) -> Self {
        let sharpe = sharpe_ratio(equity_curve);
        let sortino = sortino_ratio(equity_curve);
        let max_dd = max_drawdown(equity_curve);
        let max_dd_pct = max_drawdown_pct(equity_curve);
        let calmar = calmar_ratio(equity_curve);
        let (pf, total, wins, losses, avg_w, avg_l, lw, ll, expect, win_rate, zscore) = trade_stats(trades_pnl);
        let recovery = recovery_factor(equity_curve);
        RiskMetrics {
            sharpe,
            sortino,
            calmar,
            profit_factor: pf,
            max_drawdown: max_dd,
            max_drawdown_pct: max_dd_pct,
            expectancy: expect,
            win_rate,
            total_trades: total,
            winning_trades: wins,
            losing_trades: losses,
            avg_win: avg_w,
            avg_loss: avg_l,
            largest_win: lw,
            largest_loss: ll,
            avg_holding_time_minutes: dec!(0),
            z_score: zscore,
            recovery_factor: recovery,
        }
    }
}

/// Sharpe ratio (annualization factor = sqrt(252) for daily series).
/// Returns 0 if the sample has insufficient length or zero variance.
pub fn sharpe_ratio(equity: &[Money]) -> Decimal {
    let rets = returns(equity);
    if rets.is_empty() {
        return dec!(0);
    }
    let n = Decimal::from(rets.len());
    let mean = rets.iter().copied().sum::<Decimal>() / n;
    let var = rets.iter().map(|r| {
        let d = *r - mean;
        d * d
    }).sum::<Decimal>() / n;
    let std = var.sqrt().unwrap_or(dec!(0));
    if std.is_zero() {
        return dec!(0);
    }
    let annualization = Decimal::from(252).sqrt().unwrap_or(dec!(1));
    (mean / std) * annualization
}

/// Sortino ratio (only penalizes downside volatility).
pub fn sortino_ratio(equity: &[Money]) -> Decimal {
    let rets = returns(equity);
    if rets.is_empty() {
        return dec!(0);
    }
    let n = Decimal::from(rets.len());
    let mean = rets.iter().copied().sum::<Decimal>() / n;
    let downside: Vec<Decimal> = rets.iter().map(|r| if *r < dec!(0) { *r - dec!(0) } else { dec!(0) }).collect();
    let downside_n = Decimal::from(downside.len());
    if downside_n.is_zero() {
        return dec!(0);
    }
    let var = downside.iter().map(|d| *d * *d).sum::<Decimal>() / downside_n;
    let std = var.sqrt().unwrap_or(dec!(0));
    if std.is_zero() {
        return dec!(0);
    }
    let annualization = Decimal::from(252).sqrt().unwrap_or(dec!(1));
    (mean / std) * annualization
}

/// Calmar ratio = annualized return / maximum drawdown.
pub fn calmar_ratio(equity: &[Money]) -> Decimal {
    let max_dd = max_drawdown_pct(equity);
    if max_dd.is_zero() {
        return dec!(0);
    }
    if equity.len() < 2 {
        return dec!(0);
    }
    let first = equity.first().map(|m| m.0).unwrap_or(dec!(0));
    let last = equity.last().map(|m| m.0).unwrap_or(dec!(0));
    if first.is_zero() {
        return dec!(0);
    }
    let total_ret = (last - first) / first;
    // Simplified annualization: total_ret * (252 / n)
    let n = Decimal::from(equity.len());
    if n.is_zero() {
        return dec!(0);
    }
    let annualized = total_ret * (Decimal::from(252) / n);
    annualized / max_dd
}

/// Maximum drawdown in absolute money.
pub fn max_drawdown(equity: &[Money]) -> Decimal {
    let mut peak = dec!(0);
    let mut max_dd = dec!(0);
    for m in equity {
        if m.0 > peak {
            peak = m.0;
        }
        let dd = peak - m.0;
        if dd > max_dd {
            max_dd = dd;
        }
    }
    max_dd
}

/// Maximum drawdown as a percentage of peak.
pub fn max_drawdown_pct(equity: &[Money]) -> Decimal {
    let mut peak = dec!(0);
    let mut max_dd_pct = dec!(0);
    for m in equity {
        if m.0 > peak {
            peak = m.0;
        }
        if peak.is_zero() {
            continue;
        }
        let dd_pct = (peak - m.0) / peak;
        if dd_pct > max_dd_pct {
            max_dd_pct = dd_pct;
        }
    }
    max_dd_pct
}

/// Profit factor = sum(positive pnl) / |sum(negative pnl)|.
pub fn profit_factor(trades: &[Money]) -> Decimal {
    let gross_profit: Decimal = trades.iter().filter(|p| p.0 > dec!(0)).map(|p| p.0).sum();
    let gross_loss: Decimal = trades.iter().filter(|p| p.0 < dec!(0)).map(|p| p.0.abs()).sum();
    if gross_loss.is_zero() {
        // No losses → return profit (or 0 if no profits either).
        return if gross_profit.is_zero() { dec!(0) } else { gross_profit };
    }
    gross_profit / gross_loss
}

/// Expectancy per trade.
pub fn expectancy(trades: &[Money]) -> Decimal {
    if trades.is_empty() {
        return dec!(0);
    }
    let n = Decimal::from(trades.len());
    trades.iter().map(|p| p.0).sum::<Decimal>() / n
}

/// Recovery factor = net profit / max drawdown.
pub fn recovery_factor(equity: &[Money]) -> Decimal {
    if equity.len() < 2 {
        return dec!(0);
    }
    let first = equity.first().map(|m| m.0).unwrap_or(dec!(0));
    let last = equity.last().map(|m| m.0).unwrap_or(dec!(0));
    let profit = last - first;
    let mdd = max_drawdown(equity);
    if mdd.is_zero() {
        return dec!(0);
    }
    profit / mdd
}

/// Z-score (statistical measure of streaks).
pub fn z_score(trades: &[Money]) -> Decimal {
    if trades.len() < 10 {
        return dec!(0);
    }
    let n = Decimal::from(trades.len());
    let wins: Decimal = trades.iter().filter(|p| p.0 > dec!(0)).count().into();
    let p = wins / n;
    let q = dec!(1) - p;
    if p.is_zero() || q.is_zero() {
        return dec!(0);
    }
    // Count streaks
    let mut streaks: u32 = 0;
    let mut prev: Option<bool> = None;
    for t in trades {
        let win = t.0 > dec!(0);
        match prev {
            None => streaks += 1,
            Some(p2) if p2 != win => streaks += 1,
            _ => {}
        }
        prev = Some(win);
    }
    let r = Decimal::from(streaks);
    let expected = (dec!(2) * wins * (n - wins) / n) + dec!(1);
    let var_r = (dec!(2) * wins * (n - wins) * (dec!(2) * n - dec!(3))) / (n * n * (n - dec!(1)));
    let std_r = var_r.sqrt().unwrap_or(dec!(0));
    if std_r.is_zero() {
        return dec!(0);
    }
    (r - expected) / std_r
}

/// Converts an equity curve into a series of returns (decimal ratio).
pub fn returns(equity: &[Money]) -> Vec<Decimal> {
    equity.windows(2)
        .filter_map(|w| {
            let prev = w[0].0;
            let curr = w[1].0;
            if prev.is_zero() { None } else { Some((curr - prev) / prev) }
        })
        .collect()
}

/// Per-trade statistics: profit factor, totals, avg win/loss, etc.
#[allow(clippy::type_complexity)]
fn trade_stats(trades: &[Money]) -> (Decimal, usize, usize, usize, Decimal, Decimal, Decimal, Decimal, Decimal, Decimal, Decimal) {
    if trades.is_empty() {
        return (dec!(0), 0, 0, 0, dec!(0), dec!(0), dec!(0), dec!(0), dec!(0), dec!(0), dec!(0));
    }
    let total = trades.len();
    let wins: Vec<Decimal> = trades.iter().filter(|p| p.0 > dec!(0)).map(|p| p.0).collect();
    let losses: Vec<Decimal> = trades.iter().filter(|p| p.0 < dec!(0)).map(|p| p.0).collect();
    let n_wins = wins.len();
    let n_losses = losses.len();
    let gp: Decimal = wins.iter().sum();
    let gl: Decimal = losses.iter().map(|v| v.abs()).sum();
    let pf = if gl.is_zero() { if gp.is_zero() { dec!(0) } else { dec!(100) } } else { gp / gl };
    let avg_w = if n_wins == 0 { dec!(0) } else { gp / Decimal::from(n_wins) };
    let avg_l = if n_losses == 0 { dec!(0) } else { gl / Decimal::from(n_losses) };
    let lw = wins.iter().copied().max().unwrap_or(dec!(0));
    let ll = losses.iter().map(|v| v.abs()).max().unwrap_or(dec!(0));
    let win_rate = if total == 0 { dec!(0) } else { Decimal::from(n_wins) / Decimal::from(total) };
    let expect = expectancy(trades);
    let zs = z_score(trades);
    (pf, total, n_wins, n_losses, avg_w, avg_l, lw, ll, expect, win_rate, zs)
}

/// Validates a sample equity curve for sanity.
pub fn validate(equity: &[Money]) -> Result<(), Error> {
    if equity.is_empty() {
        return Err(Error::InvalidState("equity curve is empty".into()));
    }
    if equity.iter().any(|m| m.0 < dec!(0)) {
        return Err(Error::InvalidState("equity curve contains negative values".into()));
    }
    Ok(())
}
