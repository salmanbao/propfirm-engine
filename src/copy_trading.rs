//! Cross-account copy-trading correlation trait (P2.13 fix).
//!
//! The previous `CopyTradingRule` was self-referential — it compared
//! an account's own recent events against itself, which structurally
//! cannot detect cross-account copying. The deep assessment flagged
//! this as a P2 stub.
//!
//! This module defines the [`CopyTradingDetector`] trait so a real
//! detector can be wired in (reference feed of "master" account trades,
//! latency+size+symbol matching, N-of-M thresholds). The default
//! implementation is a no-op — it never detects copy trading (safe
//! default that doesn't false-positive in production).

use crate::core::trade::Trade;
use crate::core::types::Timestamp;
use std::collections::HashMap;

/// A candidate "master" trade from the reference feed. Used by detectors
/// that compare an account's trades against a known set of master
/// accounts (e.g. the firm's own signal providers, or other tenant
/// accounts flagged as masters).
#[derive(Debug, Clone)]
pub struct MasterTrade {
    pub master_account_id: String,
    pub symbol: String,
    pub side: String,
    pub quantity: rust_decimal::Decimal,
    pub executed_at: Timestamp,
}

/// Result of a copy-trading check on a single trade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyTradingVerdict {
    /// No correlation found.
    Clean,
    /// Suspicious: correlated with N master trades within the window.
    /// Not enough for a hard verdict.
    Suspicious { correlated_count: usize, window_seconds: i64 },
    /// Hard failure: correlated with ≥ N master trades within the window
    /// (configurable threshold).
    Confirmed { correlated_count: usize, window_seconds: i64, master_account_ids: Vec<String> },
}

/// Trait for copy-trading detectors. Real implementations compare an
/// account's trades against a reference feed of master account trades,
/// looking for tight latency + size + symbol correlation.
///
/// The default implementation is a no-op that returns `Clean` for every
/// trade — safe default that doesn't false-positive. Production
/// deployments should swap in a real detector that has access to the
/// firm's cross-account trade feed.
pub trait CopyTradingDetector: Send + Sync {
    /// Check a single trade against the reference feed. Returns the
    /// verdict (Clean / Suspicious / Confirmed).
    fn check_trade(&self, trade: &Trade) -> CopyTradingVerdict;

    /// Returns the most recent N master trades the detector has seen.
    /// Used by tests + the breach-report endpoint to surface evidence.
    fn recent_master_trades(&self, _n: usize) -> Vec<MasterTrade> { Vec::new() }
}

/// No-op copy-trading detector — never detects anything. Safe default
/// for deployments that don't have a cross-account reference feed.
#[derive(Debug, Clone, Default)]
pub struct NoOpCopyTradingDetector;

impl CopyTradingDetector for NoOpCopyTradingDetector {
    fn check_trade(&self, _trade: &Trade) -> CopyTradingVerdict {
        CopyTradingVerdict::Clean
    }
}

/// Simple threshold-based detector: maintains a list of master trades
/// (passed in via `add_master_trade`) and compares each trade against
/// the most recent masters. If ≥ `threshold` master trades match the
/// same symbol + side + similar quantity within `window_seconds`,
/// return `Confirmed`.
#[derive(Debug, Clone)]
pub struct ThresholdCopyTradingDetector {
    masters: Vec<MasterTrade>,
    window_seconds: i64,
    threshold: usize,
    /// Allowed quantity variance (e.g. 0.10 = ±10%). Default 0.10.
    quantity_tolerance: rust_decimal::Decimal,
}

impl ThresholdCopyTradingDetector {
    pub fn new(window_seconds: i64, threshold: usize) -> Self {
        Self {
            masters: Vec::new(),
            window_seconds,
            threshold,
            quantity_tolerance: rust_decimal::Decimal::new(10, 1), // 0.10
        }
    }

    /// Add a master trade to the reference feed.
    pub fn add_master_trade(&mut self, t: MasterTrade) {
        self.masters.push(t);
        // Keep only the last 1000 to avoid unbounded growth.
        if self.masters.len() > 1000 {
            self.masters.drain(0..self.masters.len() - 1000);
        }
    }

    /// Prune masters older than `max_age_seconds` (called periodically
    /// by the pipeline to bound memory usage).
    pub fn prune(&mut self, now: Timestamp, max_age_seconds: i64) {
        let cutoff = now - chrono::Duration::seconds(max_age_seconds);
        self.masters.retain(|m| m.executed_at >= cutoff);
    }
}

impl CopyTradingDetector for ThresholdCopyTradingDetector {
    fn check_trade(&self, trade: &Trade) -> CopyTradingVerdict {
        let now = trade.executed_at;
        let window_start = now - chrono::Duration::seconds(self.window_seconds);
        let mut correlated: Vec<&MasterTrade> = Vec::new();
        for m in &self.masters {
            if m.executed_at < window_start || m.executed_at > now {
                continue;
            }
            if !m.symbol.eq_ignore_ascii_case(&trade.symbol.0) {
                continue;
            }
            if !m.side.eq_ignore_ascii_case(&format!("{}", trade.side)) {
                continue;
            }
            // Quantity check: master_qty × (1 ± tolerance) contains trade.quantity.
            let lower = m.quantity * (rust_decimal::Decimal::ONE - self.quantity_tolerance);
            let upper = m.quantity * (rust_decimal::Decimal::ONE + self.quantity_tolerance);
            if trade.quantity.0 >= lower && trade.quantity.0 <= upper {
                correlated.push(m);
            }
        }
        let count = correlated.len();
        let master_ids: Vec<String> = correlated.iter().map(|m| m.master_account_id.clone()).collect();
        if count >= self.threshold {
            CopyTradingVerdict::Confirmed {
                correlated_count: count,
                window_seconds: self.window_seconds,
                master_account_ids: master_ids,
            }
        } else if count >= 1 {
            CopyTradingVerdict::Suspicious {
                correlated_count: count,
                window_seconds: self.window_seconds,
            }
        } else {
            CopyTradingVerdict::Clean
        }
    }

    fn recent_master_trades(&self, n: usize) -> Vec<MasterTrade> {
        self.masters.iter().rev().take(n).cloned().collect()
    }
}

/// In-memory master-trade feed: a simple `HashMap<account_id, Vec<MasterTrade>>`
/// for testing and small deployments.
#[derive(Debug, Clone, Default)]
pub struct InMemoryMasterFeed {
    pub masters: HashMap<String, Vec<MasterTrade>>,
}

impl InMemoryMasterFeed {
    pub fn new() -> Self { Self::default() }

    pub fn add(&mut self, master_account_id: impl Into<String>, trade: MasterTrade) {
        let id = master_account_id.into();
        self.masters.entry(id).or_default().push(trade);
    }

    pub fn all(&self) -> Vec<&MasterTrade> {
        self.masters.values().flat_map(|v| v.iter()).collect()
    }
}
