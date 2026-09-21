//! Instrument specification (§C.1 fix).
//!
//! Until now the crate had **no instrument model**: `position.rs`
//! documented `contract_size = 1` with "multipliers applied upstream"
//! (they never were), `liquidation.rs` carried a "unit-less estimate"
//! caveat, and `MaxPositionSizeRule` compared order *units* directly
//! against a *lots* limit — wrong by the contract size (×100,000 for an
//! FX major).
//!
//! An [`InstrumentSpec`] is the single source of truth for converting
//! between units and lots and for valuing notional. Specs are held in a
//! thread-safe [`InstrumentRegistry`] keyed by symbol, with a sensible
//! default for unregistered symbols. Rules, liquidation and reporting
//! all read from this one registry.

use crate::core::types::{dec, Decimal, Quantity, Symbol};
use crate::core::Error;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;

/// Specification of one trading instrument.
#[cfg_attr(
    feature = "serialization",
    derive(serde::Serialize, serde::Deserialize)
)]
#[derive(Debug, Clone)]
pub struct InstrumentSpec {
    /// Symbol this spec describes (e.g. "EURUSD").
    pub symbol: Symbol,
    /// Units per 1.0 lot (FX major: 100,000; JPY pairs also 100,000;
    /// some indices/CFDs use 1 or 10).
    pub contract_size: Decimal,
    /// Quote-currency digits used for display/pip rounding.
    pub digits: u8,
    /// Minimum price movement (a "pip" for FX; tick size generally).
    pub pip_size: Decimal,
}

impl InstrumentSpec {
    /// FX major/minor spec: 100,000 units per lot, 5 digits, 0.0001 pip.
    #[must_use]
    pub fn fx(symbol: &str) -> Self {
        InstrumentSpec {
            symbol: Symbol::new(symbol),
            contract_size: dec!(100_000),
            digits: 5,
            pip_size: dec!(0.0001),
        }
    }

    /// JPY-quote FX spec: 100,000 units per lot, 3 digits, 0.01 pip.
    #[must_use]
    pub fn fx_jpy(symbol: &str) -> Self {
        InstrumentSpec {
            symbol: Symbol::new(symbol),
            contract_size: dec!(100_000),
            digits: 3,
            pip_size: dec!(0.01),
        }
    }

    /// Generic CFD/index spec: 1 unit per lot, 2 digits, 0.01 tick.
    #[must_use]
    pub fn cfd(symbol: &str) -> Self {
        InstrumentSpec {
            symbol: Symbol::new(symbol),
            contract_size: dec!(1),
            digits: 2,
            pip_size: dec!(0.01),
        }
    }

    /// Converts a raw quantity (in units) to lots using this spec.
    #[must_use]
    pub fn units_to_lots(&self, units: Quantity) -> crate::core::types::Lots {
        crate::core::types::Lots(units.0 / self.contract_size)
    }

    /// Converts lots to raw units using this spec.
    #[must_use]
    pub fn lots_to_units(&self, lots: crate::core::types::Lots) -> Quantity {
        Quantity(lots.0 * self.contract_size)
    }

    /// Notional value in quote currency: |units × price| × contract_size
    /// divided by contract size — i.e. `lots × contract_size × price`.
    /// For FX this is the familiar `lots × 100_000 × price` quote-currency
    /// notional.
    #[must_use]
    pub fn notional(
        &self,
        lots: crate::core::types::Lots,
        price: crate::core::types::Price,
    ) -> crate::core::types::Money {
        crate::core::types::Money(lots.0 * self.contract_size * price.0)
    }
}

impl Default for InstrumentSpec {
    /// The default spec treats 1 unit = 1 lot (the crate's historical,
    /// implicit assumption — kept as the fallback so unregistered
    /// symbols behave exactly as before this module existed).
    fn default() -> Self {
        InstrumentSpec {
            symbol: Symbol::new("<default>"),
            contract_size: dec!(1),
            digits: 5,
            pip_size: dec!(0.0001),
        }
    }
}

/// Thread-safe registry of instrument specs keyed by symbol. Unknown
/// symbols fall back to the [`InstrumentSpec::default`] spec (1 unit = 1
/// lot) — the same semantics the crate had before instrument specs
/// existed, so registration is additive, never load-bearing.
#[derive(Clone, Default)]
pub struct InstrumentRegistry {
    specs: Arc<RwLock<HashMap<String, InstrumentSpec>>>,
}

impl InstrumentRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers (or replaces) the spec for a symbol.
    pub fn register(&self, spec: InstrumentSpec) {
        self.specs
            .write()
            .insert(spec.symbol.0.to_uppercase(), spec);
    }

    /// Returns the spec for a symbol, or the default spec when the
    /// symbol is unknown.
    #[must_use]
    pub fn get(&self, symbol: &Symbol) -> InstrumentSpec {
        self.specs
            .read()
            .get(&symbol.0)
            .cloned()
            .unwrap_or_default()
    }

    /// Validates that a spec is coherent (positive contract size, non-
    /// negative pip).
    pub fn validate(spec: &InstrumentSpec) -> Result<(), Error> {
        if spec.contract_size <= dec!(0) {
            return Err(Error::invalid_config(format!(
                "instrument {}: contract_size must be positive",
                spec.symbol.0
            )));
        }
        if spec.pip_size < dec!(0) {
            return Err(Error::invalid_config(format!(
                "instrument {}: pip_size cannot be negative",
                spec.symbol.0
            )));
        }
        Ok(())
    }
}

impl std::fmt::Debug for InstrumentRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.specs.read().len();
        f.debug_struct("InstrumentRegistry")
            .field("specs", &n)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fx_spec_converts_units_and_lots() {
        let spec = InstrumentSpec::fx("EURUSD");
        // 100,000 units = 1.0 lot.
        assert_eq!(spec.units_to_lots(Quantity(dec!(100_000))).0, dec!(1));
        assert_eq!(
            spec.lots_to_units(crate::core::types::Lots(dec!(1))).0,
            dec!(100_000)
        );
        // 0.1 lot = 10,000 units.
        assert_eq!(
            spec.lots_to_units(crate::core::types::Lots(dec!(0.1))).0,
            dec!(10_000)
        );
    }

    #[test]
    fn cfd_spec_identity_conversion() {
        let spec = InstrumentSpec::cfd("XAUUSD");
        assert_eq!(spec.units_to_lots(Quantity(dec!(5))).0, dec!(5));
    }

    #[test]
    fn registry_falls_back_to_default_spec() {
        let reg = InstrumentRegistry::new();
        let spec = reg.get(&Symbol::new("UNKNOWN"));
        assert_eq!(
            spec.contract_size,
            dec!(1),
            "unknown symbols use 1 unit = 1 lot"
        );
    }

    #[test]
    fn registry_register_and_get() {
        let reg = InstrumentRegistry::new();
        reg.register(InstrumentSpec::fx("EURUSD"));
        assert_eq!(reg.get(&Symbol::new("eurusd")).contract_size, dec!(100_000));
    }

    #[test]
    fn notional_uses_contract_size() {
        let spec = InstrumentSpec::fx("EURUSD");
        let n = spec.notional(
            crate::core::types::Lots(dec!(1)),
            crate::core::types::Price(dec!(1.08)),
        );
        assert_eq!(n.0, dec!(108_000));
    }
}
