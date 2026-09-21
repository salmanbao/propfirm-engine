//! **§E.2 fix**: Prometheus metrics exported in Prometheus text format.
//!
//! Metrics collected:
//! - `propfirm_evaluation_duration_seconds` (histogram): end-to-end evaluation
//!   latency per account/tick, with labels `account_id`, `tenant_id`.
//! - `propfirm_tick_lag_seconds` (histogram): wall-clock lag between tick
//!   timestamp and evaluation time, labeled by `account_id`.
//! - `propfirm_breach_total` (counter): number of breach verdicts emitted,
//!   labeled by `violation_kind`, `severity`.
//! - `propfirm_rule_evaluation_total` (counter): per-rule evaluation results,
//!   labeled by `rule_name`, `result` (pass/warn/fail/liquidate/emergency).
//! - `propfirm_http_requests_total` (counter): HTTP requests by method, path,
//!   status_code.
//! - `propfirm_http_request_duration_seconds` (histogram): HTTP request
//!   latency by method, path.

use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;

// ---------------------------------------------------------------------------
// Shared metric state
// ---------------------------------------------------------------------------

/// Accumulated Prometheus-formatted metrics state.
pub struct Metrics {
    /// Evaluation latency samples (seconds), per account.
    evaluation_durations: RwLock<Vec<(String, String, f64)>>,
    /// Tick lag samples (seconds), per account.
    tick_lags: RwLock<Vec<(String, f64)>>,
    /// Breach counts per violation kind + severity.
    breaches: RwLock<HashMap<(String, String), u64>>,
    /// Rule evaluation counts per rule name + result.
    rule_evals: RwLock<HashMap<(String, String), u64>>,
    /// HTTP request counts per method + path + status.
    http_requests: RwLock<HashMap<(String, String, u16), u64>>,
    /// HTTP request latency samples per method + path.
    http_durations: RwLock<Vec<(String, String, f64)>>,
}

impl Metrics {
    pub fn new() -> Self {
        Self {
            evaluation_durations: RwLock::new(Vec::new()),
            tick_lags: RwLock::new(Vec::new()),
            breaches: RwLock::new(HashMap::new()),
            rule_evals: RwLock::new(HashMap::new()),
            http_requests: RwLock::new(HashMap::new()),
            http_durations: RwLock::new(Vec::new()),
        }
    }

    /// Record an evaluation duration sample.
    pub fn record_evaluation_duration(&self, account_id: String, tenant_id: String, dur: f64) {
        self.evaluation_durations.write().push((account_id, tenant_id, dur));
        // Keep bounded: drop oldest if > 10k samples.
        let mut buf = self.evaluation_durations.write();
        if buf.len() > 10_000 {
            buf.remove(0);
        }
    }

    /// Record a tick lag sample.
    pub fn record_tick_lag(&self, account_id: String, lag: f64) {
        self.tick_lags.write().push((account_id, lag));
        let mut buf = self.tick_lags.write();
        if buf.len() > 10_000 {
            buf.remove(0);
        }
    }

    /// Increment breach counter for a violation kind + severity.
    pub fn record_breach(&self, kind: &str, severity: &str) {
        let key = (kind.to_string(), severity.to_string());
        *self.breaches.write().entry(key).or_insert(0) += 1;
    }

    /// Increment rule evaluation counter for a rule name + result.
    pub fn record_rule_eval(&self, rule_name: &str, result: &str) {
        let key = (rule_name.to_string(), result.to_string());
        *self.rule_evals.write().entry(key).or_insert(0) += 1;
    }

    /// Increment HTTP request counter.
    pub fn record_http_request(&self, method: &str, path: &str, status: u16) {
        let key = (method.to_string(), path.to_string(), status);
        *self.http_requests.write().entry(key).or_insert(0) += 1;
    }

    /// Record an HTTP request duration sample.
    pub fn record_http_duration(&self, method: &str, path: &str, dur: f64) {
        self.http_durations.write().push((method.to_string(), path.to_string(), dur));
        let mut buf = self.http_durations.write();
        if buf.len() > 10_000 {
            buf.remove(0);
        }
    }

    /// Compute approximate p50 and p99 from a sorted slice of durations.
    fn percentile(durations: &[f64], p: f64) -> Option<f64> {
        if durations.is_empty() {
            return None;
        }
        let mut sorted: Vec<f64> = durations.to_vec();
        sorted.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = ((p / 100.0) * (sorted.len() as f64)) as usize;
        let idx = idx.min(sorted.len() - 1);
        Some(sorted[idx])
    }

    /// Render all metrics in Prometheus text exposition format.
    pub fn render(&self) -> String {
        let mut out = String::new();

        // --- Evaluation duration histogram ---
        out.push_str("# HELP propfirm_evaluation_duration_seconds Evaluation latency (end-to-end per account/tick)\n");
        out.push_str("# TYPE propfirm_evaluation_duration_seconds histogram\n");
        {
            let samples = self.evaluation_durations.read();
            if !samples.is_empty() {
                let mut by_account: HashMap<(String, String), Vec<f64>> = HashMap::new();
                for (acc, tenant, dur) in samples.iter() {
                    by_account.entry((acc.clone(), tenant.clone())).or_default().push(*dur);
                }
                // Emit as a summary-like approximation: count + p50 + p99.
                // (Full histogram buckets are expensive to compute on every scrape;
                // we emit count + quantiles for the hot path.)
                for ((acc, tenant), durs) in &by_account {
                    let count = durs.len() as u64;
                    let p50 = Self::percentile(durs, 50.0).unwrap_or(0.0);
                    let p99 = Self::percentile(durs, 99.0).unwrap_or(0.0);
                    out.push_str(&format!(
                        "propfirm_evaluation_duration_seconds_count{{account_id=\"{}\",tenant_id=\"{}\"}} {count}\n",
                        acc, tenant
                    ));
                    out.push_str(&format!(
                        "propfirm_evaluation_duration_seconds_p50{{account_id=\"{}\",tenant_id=\"{}\"}} {p50:.6}\n",
                        acc, tenant
                    ));
                    out.push_str(&format!(
                        "propfirm_evaluation_duration_seconds_p99{{account_id=\"{}\",tenant_id=\"{}\"}} {p99:.6}\n",
                        acc, tenant
                    ));
                }
                // Global aggregates.
                let all: Vec<f64> = samples.iter().map(|(_, _, d)| *d).collect();
                let count = all.len() as u64;
                let p50 = Self::percentile(&all, 50.0).unwrap_or(0.0);
                let p99 = Self::percentile(&all, 99.0).unwrap_or(0.0);
                out.push_str(&format!("propfirm_evaluation_duration_seconds_count{{global=\"1\"}} {count}\n"));
                out.push_str(&format!("propfirm_evaluation_duration_seconds_p50{{global=\"1\"}} {p50:.6}\n"));
                out.push_str(&format!("propfirm_evaluation_duration_seconds_p99{{global=\"1\"}} {p99:.6}\n"));
            }
        }

        // --- Tick lag histogram ---
        out.push_str("# HELP propfirm_tick_lag_seconds Wall-clock lag between tick timestamp and evaluation time\n");
        out.push_str("# TYPE propfirm_tick_lag_seconds histogram\n");
        {
            let samples = self.tick_lags.read();
            if !samples.is_empty() {
                let all: Vec<f64> = samples.iter().map(|(_, d)| *d).collect();
                let count = all.len() as u64;
                let p50 = Self::percentile(&all, 50.0).unwrap_or(0.0);
                let p99 = Self::percentile(&all, 99.0).unwrap_or(0.0);
                out.push_str(&format!("propfirm_tick_lag_seconds_count {{count}}\n", count = count));
                out.push_str(&format!("propfirm_tick_lag_seconds_p50 {{p50:.6}}\n", p50 = p50));
                out.push_str(&format!("propfirm_tick_lag_seconds_p99 {{p99:.6}}\n", p99 = p99));
            }
        }

        // --- Breach counter ---
        out.push_str("# HELP propfirm_breach_total Number of breach verdicts emitted\n");
        out.push_str("# TYPE propfirm_breach_total counter\n");
        {
            let breaches = self.breaches.read();
            if !breaches.is_empty() {
                for ((kind, severity), count) in breaches.iter() {
                    out.push_str(&format!(
                        "propfirm_breach_total{{violation_kind=\"{}\",severity=\"{}\"}} {count}\n",
                        kind, severity
                    ));
                }
            }
        }

        // --- Rule evaluation counter ---
        out.push_str("# HELP propfirm_rule_evaluation_total Number of rule evaluations by result\n");
        out.push_str("# TYPE propfirm_rule_evaluation_total counter\n");
        {
            let evals = self.rule_evals.read();
            if !evals.is_empty() {
                for ((rule_name, result), count) in evals.iter() {
                    out.push_str(&format!(
                        "propfirm_rule_evaluation_total{{rule_name=\"{}\",result=\"{}\"}} {count}\n",
                        rule_name, result
                    ));
                }
            }
        }

        // --- HTTP request counter ---
        out.push_str("# HELP propfirm_http_requests_total Total HTTP requests by method, path, status\n");
        out.push_str("# TYPE propfirm_http_requests_total counter\n");
        {
            let reqs = self.http_requests.read();
            if !reqs.is_empty() {
                for ((method, path, status), count) in reqs.iter() {
                    out.push_str(&format!(
                        "propfirm_http_requests_total{{method=\"{}\",path=\"{}\",status_code=\"{}\"}} {count}\n",
                        method, path, status
                    ));
                }
            }
        }

        // --- HTTP request duration histogram ---
        out.push_str("# HELP propfirm_http_request_duration_seconds HTTP request latency\n");
        out.push_str("# TYPE propfirm_http_request_duration_seconds histogram\n");
        {
            let samples = self.http_durations.read();
            if !samples.is_empty() {
                let mut by_route: HashMap<(String, String), Vec<f64>> = HashMap::new();
                for (method, path, dur) in samples.iter() {
                    by_route.entry((method.clone(), path.clone())).or_default().push(*dur);
                }
                for ((method, path), durs) in &by_route {
                    let count = durs.len() as u64;
                    let p50 = Self::percentile(durs, 50.0).unwrap_or(0.0);
                    let p99 = Self::percentile(durs, 99.0).unwrap_or(0.0);
                    out.push_str(&format!(
                        "propfirm_http_request_duration_seconds_count{{method=\"{}\",path=\"{}\"}} {count}\n",
                        method, path
                    ));
                    out.push_str(&format!(
                        "propfirm_http_request_duration_seconds_p50{{method=\"{}\",path=\"{}\"}} {p50:.6}\n",
                        method, path
                    ));
                    out.push_str(&format!(
                        "propfirm_http_request_duration_seconds_p99{{method=\"{}\",path=\"{}\"}} {p99:.6}\n",
                        method, path
                    ));
                }
            }
        }

        out
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Shared metrics handle, cloneable via Arc.
pub type SharedMetrics = Arc<Metrics>;
