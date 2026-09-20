//! Exposure analytics: gross/net exposure, concentration, risk-by-symbol.

use crate::core::position::Position;
use crate::core::tick::Quote;
use crate::core::types::{Decimal, Money, dec};
use std::collections::HashMap;

/// Aggregated exposure metrics for a portfolio of open positions.
#[derive(Debug, Clone, Default)]
pub struct Exposure {
    pub gross: Money,
    pub net: Money,
    pub long_count: u32,
    pub short_count: u32,
    pub long_volume: Decimal,
    pub short_volume: Decimal,
    pub net_volume: Decimal,
    pub symbol_concentration: HashMap<String, Decimal>,
}

impl Exposure {
    /// Computes exposure for a slice of open positions using a per-symbol
    /// quote map.
    pub fn compute(positions: &[Position], quotes: &HashMap<String, Quote>) -> Self {
        let mut gross = dec!(0);
        let mut net = dec!(0);
        let mut long_count = 0u32;
        let mut short_count = 0u32;
        let mut long_volume = dec!(0);
        let mut short_volume = dec!(0);
        let mut net_volume = dec!(0);
        let mut concentration: HashMap<String, Decimal> = HashMap::new();

        for p in positions.iter().filter(|p| p.is_open()) {
            let quote = match quotes.get(&p.symbol.0) {
                Some(q) => q,
                None => continue,
            };
            let price = quote.mid().0;
            let notional = price * p.open_quantity.0;
            gross += notional;
            match p.side {
                crate::core::position::PositionSide::Long => {
                    net += notional;
                    long_count += 1;
                    long_volume += p.open_quantity.0;
                    net_volume += p.open_quantity.0;
                }
                crate::core::position::PositionSide::Short => {
                    net -= notional;
                    short_count += 1;
                    short_volume += p.open_quantity.0;
                    net_volume -= p.open_quantity.0;
                }
            }
            *concentration.entry(p.symbol.0.clone()).or_insert(dec!(0)) += notional;
        }
        Exposure {
            gross: Money(gross),
            net: Money(net),
            long_count,
            short_count,
            long_volume,
            short_volume,
            net_volume,
            symbol_concentration: concentration,
        }
    }

    /// Returns the most concentrated symbol and its share of total gross.
    pub fn top_symbol(&self) -> Option<(&str, Decimal)> {
        if self.gross.0.is_zero() {
            return None;
        }
        self.symbol_concentration
            .iter()
            .map(|(k, v)| (k.as_str(), *v / self.gross.0))
            .max_by(|a, b| a.1.cmp(&b.1))
    }
}
