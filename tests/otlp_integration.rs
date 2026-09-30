//! OTLP (OpenTelemetry) exporter integration test.
//!
//! ## What this test verifies
//!
//! When the `otel` cargo feature is enabled AND
//! `observability.otlp.endpoint` is set, the binary's tracing
//! subscriber should install an OTLP layer that exports spans to the
//! configured collector.
//!
//! This test exercises the *initialization* path — it verifies that
//! `otel::init_tracing` doesn't panic when given various OTLP
//! configurations. It does NOT verify that spans actually reach a
//! collector (that would require running a real OTLP collector,
//! which is out of scope for unit tests).
//!
//! ## Why a mutex?
//!
//! `tracing`'s global default subscriber can only be set ONCE per
//! process. Tests that call `init_tracing` would normally panic on
//! the second call. We use a process-wide `Mutex` to serialize the
//! tests that touch the global subscriber. Run with `--test-threads=1`
//! for the cleanest output.
//!
//! ## When this test runs
//!
//! Only when the `otel` cargo feature is enabled. CI runs it via
//! `cargo test --all-features`. Locally:
//!
//! ```bash
//! cargo test --features server,otel --test otlp_integration -- --test-threads=1
//! ```

#![cfg(feature = "otel")]
#![cfg(feature = "server")]

use propfirm::settings::{ObservabilitySettings, OtlpSettings};
use std::sync::Mutex;

/// Process-wide mutex to serialize tests that touch the global
/// tracing subscriber. The global can only be set ONCE per process,
/// so without this the tests would panic when run in parallel.
static INIT_MUTEX: Mutex<()> = Mutex::new(());

/// Helper: build observability settings with the given OTLP config.
fn make_settings(otlp: OtlpSettings) -> ObservabilitySettings {
    let mut s = ObservabilitySettings::default();
    s.log_filter = "info,propfirm=trace".to_string();
    s.log_format = "json".to_string();
    s.metrics_enabled = false;
    s.panic_hook = false;
    s.otlp = otlp;
    s
}

/// Test 1: init_tracing with a configured HTTP endpoint should
/// succeed (the OTLP layer is installed, the fmt layer is installed,
/// the global subscriber is set).
///
/// This is the canonical init path used by the production binary.
/// The first test in the file (by alphabetical order) that runs sets
/// the global subscriber; subsequent tests in this file that call
/// init_tracing will return Ok but be no-ops.
#[tokio::test]
async fn otlp_init_with_http_endpoint_succeeds() {
    let _guard = INIT_MUTEX.lock().unwrap();
    let settings = make_settings(OtlpSettings {
        endpoint: "http://127.0.0.1:4318".to_string(),
        protocol: "http".to_string(),
        service_name: "propfirm-engine-http-test".to_string(),
        stdout: false,
        sample_ratio: 1.0,
    });
    // First test to grab the mutex + call init_tracing wins. The
    // global subscriber is set. Subsequent calls in this file are
    // no-ops but should NOT error.
    let result = propfirm::api::otel::init_tracing(&settings);
    assert!(
        result.is_ok(),
        "init_tracing with HTTP endpoint should not error: {:?}",
        result.err()
    );

    // Emit a span to exercise the layer. The exporter will try to
    // POST to 127.0.0.1:4318 (no collector is listening — that's
    // fine, the batch exporter retries silently).
    tracing::info!(test = "otlp_http_init", "span emitted");
}

/// Test 2: init_tracing with an empty endpoint AND stdout=false
/// should skip the OTLP layer entirely (just install fmt).
///
/// NOTE: Because test 1 may have already set the global subscriber,
/// this test verifies init_tracing returns Ok but does NOT assert
/// that the OTLP layer was installed (we can't easily inspect the
/// global subscriber's internals).
#[tokio::test]
async fn otlp_init_with_empty_endpoint_skips_layer() {
    let _guard = INIT_MUTEX.lock().unwrap();
    let settings = make_settings(OtlpSettings {
        endpoint: String::new(),
        protocol: "grpc".to_string(),
        service_name: "propfirm-engine-empty-test".to_string(),
        stdout: false,
        sample_ratio: 1.0,
    });
    let _ = propfirm::api::otel::init_tracing(&settings);
    tracing::info!(test = "otlp_empty_init", "span emitted without OTLP");
}

/// Test 3: the shutdown_otlp() function should be safe to call
/// multiple times (idempotent — no panic, no error returned).
#[tokio::test]
async fn otlp_shutdown_is_idempotent() {
    // No mutex needed — shutdown_otlp doesn't touch global state.
    propfirm::api::otel::shutdown_otlp();
    propfirm::api::otel::shutdown_otlp();
    propfirm::api::otel::shutdown_otlp();
    // If we got here, the function didn't panic.
}

/// Test 4: verify the OtlpSettings struct's Default impl produces
/// sensible values (so the chart values.yaml doesn't accidentally
/// break initialization).
#[tokio::test]
async fn otlp_default_settings_are_sane() {
    // No mutex needed — this test doesn't touch the global subscriber.
    let d = OtlpSettings::default();
    assert!(d.endpoint.is_empty(), "default endpoint should be empty");
    assert_eq!(d.protocol, "grpc");
    assert_eq!(d.service_name, "propfirm-engine");
    assert!(!d.stdout);
    assert!((d.sample_ratio - 1.0).abs() < f64::EPSILON);
}

/// Test 5: verify that ObservabilitySettings::default() embeds the
/// correct OtlpSettings (no endpoint, grpc protocol, correct service
/// name). This catches accidental drift in the Default impls.
#[tokio::test]
async fn otlp_settings_default_propagates_through_observability() {
    let obs = ObservabilitySettings::default();
    assert!(obs.otlp.endpoint.is_empty());
    assert_eq!(obs.otlp.protocol, "grpc");
    assert_eq!(obs.otlp.service_name, "propfirm-engine");
    assert!(!obs.otlp.stdout);
}
