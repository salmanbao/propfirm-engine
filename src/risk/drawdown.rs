//! Drawdown helpers.

use crate::core::types::{Decimal, Money, dec};

/// Returns the largest peak-to-trough drawdown in money terms.
pub fn max_drawdown(equity: &[Money]) -> Decimal {
    let mut peak = dec!(0);
    let mut mdd = dec!(0);
    for m in equity {
        if m.0 > peak {
            peak = m.0;
        }
        let dd = peak - m.0;
        if dd > mdd {
            mdd = dd;
        }
    }
    mdd
}

/// Returns the duration (in samples) of the longest drawdown.
pub fn max_drawdown_duration(equity: &[Money]) -> u32 {
    let mut peak = dec!(0);
    let mut start: Option<usize> = None;
    let mut longest = 0u32;
    for (i, m) in equity.iter().enumerate() {
        if m.0 > peak {
            peak = m.0;
            if let Some(s) = start {
                let len = (i - s) as u32;
                if len > longest {
                    longest = len;
                }
                start = None;
            }
        } else if start.is_none() && peak > dec!(0) {
            start = Some(i - 1);
        }
    }
    if let Some(s) = start {
        let len = (equity.len() - s) as u32;
        if len > longest {
            longest = len;
        }
    }
    longest
}
