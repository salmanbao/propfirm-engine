//! Value-at-Risk (parametric, Gaussian).

use crate::core::types::{Decimal, Money, dec};
use rust_decimal::MathematicalOps;

/// Parametric VaR using a Gaussian assumption.
/// Returns the expected loss at the given confidence level over the given
/// horizon (in days), given a vector of daily returns.
pub fn parametric_var(returns: &[Decimal], confidence: Decimal, horizon_days: u32) -> Money {
    if returns.is_empty() {
        return Money::ZERO;
    }
    let n = Decimal::from(returns.len());
    let mean = returns.iter().copied().sum::<Decimal>() / n;
    let var = returns.iter().map(|r| {
        let d = *r - mean;
        d * d
    }).sum::<Decimal>() / n;
    let std = var.sqrt().unwrap_or(dec!(0));
    if std.is_zero() {
        return Money::ZERO;
    }
    // Inverse normal approximation: 95% → 1.645, 99% → 2.326.
    let z = if (confidence - dec!(0.95)).abs() < dec!(0.001) {
        dec!(1.645)
    } else if (confidence - dec!(0.99)).abs() < dec!(0.001) {
        dec!(2.326)
    } else {
        // Linear approx between 95% and 99%
        if confidence >= dec!(0.95) && confidence <= dec!(0.99) {
            let t = (confidence - dec!(0.95)) / (dec!(0.99) - dec!(0.95));
            dec!(1.645) + t * (dec!(2.326) - dec!(1.645))
        } else {
            dec!(2) // conservative
        }
    };
    let horizon_scale = Decimal::from(horizon_days).sqrt().unwrap_or(dec!(1));
    let var_pct = (mean * horizon_scale) + (z * std * horizon_scale);
    Money(var_pct.abs())
}

/// Expected Shortfall (ES) using the same Gaussian assumption.
pub fn expected_shortfall(returns: &[Decimal], confidence: Decimal, horizon_days: u32) -> Money {
    if returns.is_empty() {
        return Money::ZERO;
    }
    let var = parametric_var(returns, confidence, horizon_days);
    let n = Decimal::from(returns.len());
    let mean = returns.iter().copied().sum::<Decimal>() / n;
    let var2 = returns.iter().map(|r| {
        let d = *r - mean;
        d * d
    }).sum::<Decimal>() / n;
    let std = var2.sqrt().unwrap_or(dec!(0));
    if std.is_zero() {
        return Money::ZERO;
    }
    // ES = mean + std * phi(z) / (1 - confidence)
    let z = if (confidence - dec!(0.95)).abs() < dec!(0.001) {
        dec!(1.645)
    } else if (confidence - dec!(0.99)).abs() < dec!(0.001) {
        dec!(2.326)
    } else {
        dec!(2)
    };
    let horizon_scale = Decimal::from(horizon_days).sqrt().unwrap_or(dec!(1));
    // pdf at z = exp(-z²/2) / sqrt(2π)
    let pdf = (dec!(-0.5) * z * z).exp() / (dec!(2) * Decimal::PI).sqrt().unwrap_or(dec!(1));
    let es_pct = (mean * horizon_scale) + (std * horizon_scale * pdf / (dec!(1) - confidence));
    let _ = var;
    Money(es_pct.abs())
}
