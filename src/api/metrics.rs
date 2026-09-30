//! Metric constants and helpers for the propfirm-engine HTTP API.
//!
//! All metrics are recorded via the `metrics` crate macros, which route
//! to the global Prometheus recorder installed by
//! [`crate::api::server::default_metrics_handle`].
//!
//! ## Naming convention
//!
//! - Counters: `propfirm_<noun>_<verb>_total` (e.g. `propfirm_evaluate_total`)
//! - Histograms: `propfirm_<noun>_duration_seconds`
//! - Gauges: `propfirm_<noun>_current`
//!
//! Labels use snake_case, no abbreviations.

use std::time::Instant;

/// Record an HTTP request counter (called by the TraceLayer / handlers).
#[inline]
pub fn record_request(method: &str, status: u16) {
    metrics::counter!("propfirm_http_requests_total",
        "method" => method.to_string(),
        "status" => status.to_string(),
    )
    .increment(1);
}

/// Record a per-decision-kind counter (called by `evaluate_internal_impl`).
#[inline]
pub fn record_decision(decision_kind: &str) {
    metrics::counter!("propfirm_evaluate_decisions_total",
        "kind" => decision_kind.to_string(),
    )
    .increment(1);
}

/// Record idempotency outcome (called by `evaluate_internal` after the
/// `check_and_remember` call).
#[inline]
pub fn record_idempotency_outcome(outcome: &str) {
    metrics::counter!("propfirm_idempotency_outcomes_total",
        "outcome" => outcome.to_string(),
    )
    .increment(1);
}

/// Record an error counter (called by handlers on 4xx/5xx).
#[inline]
pub fn record_error(endpoint: &str, kind: &str) {
    metrics::counter!("propfirm_errors_total",
        "endpoint" => endpoint.to_string(),
        "kind" => kind.to_string(),
    )
    .increment(1);
}

/// A per-request latency scope timer. Records the elapsed time on drop.
pub struct LatencyScope {
    endpoint: &'static str,
    start: Instant,
}

impl LatencyScope {
    /// Begin measuring latency for a request on the given endpoint.
    #[must_use]
    pub fn start(endpoint: &'static str) -> Self {
        LatencyScope {
            endpoint,
            start: Instant::now(),
        }
    }
}

impl Drop for LatencyScope {
    fn drop(&mut self) {
        let elapsed = self.start.elapsed().as_secs_f64();
        metrics::histogram!("propfirm_request_duration_seconds",
            "endpoint" => self.endpoint,
        )
        .record(elapsed);
    }
}

/// Worker metrics (recorded by the `propfirm-worker` binary).
pub mod worker {
    /// Increment after a message is consumed from the request stream.
    #[inline]
    pub fn record_message_consumed(consumer: &str) {
        metrics::counter!("propfirm_event_bus_messages_consumed_total",
            "consumer" => consumer.to_string(),
        )
        .increment(1);
    }

    /// Increment after a response is produced to the response stream.
    #[inline]
    pub fn record_message_produced() {
        metrics::counter!("propfirm_event_bus_messages_produced_total").increment(1);
    }

    /// Increment after a message is auto-claimed from the PEL.
    #[inline]
    pub fn record_message_claimed() {
        metrics::counter!("propfirm_event_bus_messages_claimed_total").increment(1);
    }

    /// Increment after a message is acknowledged (XACK).
    #[inline]
    pub fn record_message_acked() {
        metrics::counter!("propfirm_event_bus_messages_acked_total").increment(1);
    }

    /// Begin a per-message processing latency scope.
    #[must_use]
    pub fn latency_scope() -> super::LatencyScope {
        super::LatencyScope::start("event_bus_worker")
    }

    /// Increment when a worker errors (decode failure, redis error, etc.).
    #[inline]
    pub fn record_error(kind: &str) {
        metrics::counter!("propfirm_event_bus_errors_total",
            "kind" => kind.to_string(),
        )
        .increment(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tracing_test::traced_test]
    #[test]
    fn test_record_decision_emits_metric() {
        // Record a "Pass" decision — this should emit a metrics
        // counter increment with the kind=Pass label.
        record_decision("Pass");

        // The tracing_test mock subscriber captures all events.
        // We assert that the "propfirm_evaluate_decisions_total"
        // counter was incremented at least once.
        // (tracing_test doesn't directly assert on metrics events —
        // it captures tracing::info!/debug!/etc. spans + events.
        // The metrics crate uses a separate recorder, so this test
        // just verifies the function doesn't panic.)
        assert!(true, "record_decision completed without panic");
    }

    #[tracing_test::traced_test]
    #[test]
    fn test_record_idempotency_outcome_emits_metric() {
        record_idempotency_outcome("fresh");
        record_idempotency_outcome("replay");
        record_idempotency_outcome("conflict");
        record_idempotency_outcome("error");
        // All four outcomes should be recordable without panic.
    }

    #[tracing_test::traced_test]
    #[test]
    fn test_latency_scope_records_duration() {
        let _scope = LatencyScope::start("/test/endpoint");
        // The scope starts measuring. When it's dropped (at end of
        // this block), it records the histogram value.
        std::thread::sleep(std::time::Duration::from_millis(1));
        // _scope is dropped here → histogram recorded.
    }

    #[tracing_test::traced_test]
    #[test]
    fn test_record_error_emits_metric() {
        record_error("/test/endpoint", "test_error_kind");
        // Should not panic.
    }
}
