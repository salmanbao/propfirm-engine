//! End-to-end OTLP test — runs against a real OpenTelemetry Collector.
//!
//! ## What this test does
//!
//! 1. Verifies the `OTLP_COLLECTOR_ENDPOINT` env var is set
//!    (otherwise skips — this test is only run from CI where the
//!    collector is brought up as a service container).
//! 2. Initializes tracing with the OTLP layer pointing at the
//!    collector via gRPC.
//! 3. Emits a span: `tracing::info!("e2e otlp test span")`.
//! 4. Polls the collector's Prometheus `/metrics` endpoint
//!    (default `http://collector:8888/metrics`) for up to 15s,
//!    looking for `otelcol_receiver_accepted_spans` to increment.
//! 5. Asserts that the collector received at least 1 span.
//!
//! ## When this test runs
//!
//! - CI: the `otel-e2e` job in `.github/workflows/ci.yml` brings
//!   up an `otel/opentelemetry-collector` service container + runs
//!   `cargo test --features server,otel --test otlp_e2e` with
//!   `OTLP_COLLECTOR_ENDPOINT=http://localhost:4317` and
//!   `OTLP_COLLECTOR_METRICS=http://localhost:8888/metrics`.
//! - Locally: run with
//!   ```bash
//!   docker run -d --name otelcol -p 4317:4317 -p 8888:8888 \
//!     otel/opentelemetry-collector
//!   OTLP_COLLECTOR_ENDPOINT=http://localhost:4317 \
//!   OTLP_COLLECTOR_METRICS=http://localhost:8888/metrics \
//!   cargo test --features server,otel --test otlp_e2e -- --nocapture
//!   ```
//!
//! ## Why this is separate from `otlp_integration.rs`
//!
//! `otlp_integration.rs` tests the init path without a real
//! collector (uses try_init + verifies no panic). That test runs
//! on every PR.
//!
//! This file (`otlp_e2e.rs`) tests the actual span-export path
//! against a real collector. It's slower (waits up to 15s for the
//! batch flush) and needs Docker/collector, so it's gated on the
//! `OTLP_COLLECTOR_ENDPOINT` env var. When that env var is absent,
//! the test is marked ignored (not failed), so `cargo test --all
//! -features` doesn't break when there's no collector available.

#![cfg(feature = "otel")]
#![cfg(feature = "server")]

use std::time::Duration;

/// Get the OTLP gRPC endpoint from env. Returns None when not set
/// (in which case the test is skipped — see `#[ignore]` below).
fn collector_endpoint() -> Option<String> {
    std::env::var("OTLP_COLLECTOR_ENDPOINT").ok()
}

/// Get the collector's Prometheus metrics URL (for asserting spans
/// were received). Defaults to `http://localhost:8888/metrics`.
fn collector_metrics_url() -> String {
    std::env::var("OTLP_COLLECTOR_METRICS")
        .unwrap_or_else(|_| "http://localhost:8888/metrics".to_string())
}

/// Query the collector's `/metrics` endpoint and return the raw text.
async fn fetch_metrics(url: &str) -> Option<String> {
    // We use a hand-rolled HTTP client to avoid pulling in reqwest
    // (the binary already has it via the `otel` feature, but linking
    // it in tests would inflate compile times).
    //
    // For simplicity + correctness, use the `reqwest` crate if
    // available; otherwise fall back to a hand-rolled TCP HTTP GET.
    let parsed = url::parse_url(url).ok()?;
    let mut stream = tokio::net::TcpStream::connect((parsed.host.as_str(), parsed.port))
        .await
        .ok()?;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        parsed.path, parsed.host
    );
    stream.write_all(request.as_bytes()).await.ok()?;
    let mut buf = Vec::with_capacity(8192);
    stream.read_to_end(&mut buf).await.ok()?;
    let body = String::from_utf8_lossy(&buf);
    // Strip HTTP headers — the body starts after the first blank line.
    let body_start = body.find("\r\n\r\n").map(|i| i + 4).unwrap_or(0);
    Some(body[body_start..].to_string())
}

/// Tiny URL parser — extracts host, port, path from `http://host:port/path`.
mod url {
    pub struct ParsedUrl {
        pub host: String,
        pub port: u16,
        pub path: String,
    }

    pub fn parse_url(url: &str) -> Result<ParsedUrl, String> {
        let url = url
            .strip_prefix("http://")
            .ok_or("expected http:// prefix")?;
        let (host_port, path) = match url.find('/') {
            Some(i) => (&url[..i], &url[i..]),
            None => (url, "/"),
        };
        let (host, port) = match host_port.find(':') {
            Some(i) => {
                let h = &host_port[..i];
                let p: u16 = host_port[i + 1..].parse().map_err(|_| "invalid port")?;
                (h.to_string(), p)
            }
            None => (host_port.to_string(), 80),
        };
        Ok(ParsedUrl {
            host,
            port,
            path: path.to_string(),
        })
    }
}

/// Poll the collector's `/metrics` endpoint for up to `timeout`
/// seconds, looking for `otelcol_receiver_accepted_spans` to be
/// non-zero.
async fn wait_for_spans(timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if let Some(metrics) = fetch_metrics(&collector_metrics_url()).await {
            if metrics
                .lines()
                .filter(|l| l.starts_with("otelcol_receiver_accepted_spans"))
                .filter_map(|l| l.split_whitespace().nth(1))
                .filter_map(|v| v.parse::<f64>().ok())
                .any(|v| v > 0.0)
            {
                return true;
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    false
}

#[tokio::test]
#[ignore = "requires OTLP_COLLECTOR_ENDPOINT env var — run from CI otlp-e2e job"]
async fn otlp_e2e_spans_reach_collector() {
    let endpoint = match collector_endpoint() {
        Some(e) => e,
        None => {
            eprintln!("SKIP: OTLP_COLLECTATOR_ENDPOINT not set; run from CI otlp-e2e job");
            return;
        }
    };
    eprintln!("OTLP collector endpoint: {endpoint}");

    // Initialize tracing with the OTLP layer pointing at the real
    // collector (gRPC transport, default 5s batch flush).
    use propfirm::settings::{ObservabilitySettings, OtlpSettings};
    let settings = ObservabilitySettings {
        log_filter: "info,propfirm=trace".to_string(),
        log_format: "json".to_string(),
        metrics_enabled: false,
        metrics_path: "/metrics".to_string(),
        panic_hook: false,
        otlp: OtlpSettings {
            endpoint: endpoint.clone(),
            protocol: "grpc".to_string(),
            service_name: "propfirm-engine-e2e-test".to_string(),
            stdout: false,
            sample_ratio: 1.0,
        },
        flame_output_path: String::new(),
    };
    propfirm::api::otel::init_tracing(&settings).expect("init_tracing");

    // Emit a span. The OTLP exporter batches — flush happens
    // 5s later by default. We wait up to 15s.
    tracing::info!(
        otel.endpoint = %endpoint,
        test = "otlp_e2e",
        "end-to-end OTLP test span"
    );
    eprintln!("emitted span; polling collector for receiver_accepted_spans…");

    let ok = wait_for_spans(Duration::from_secs(15)).await;
    propfirm::api::otel::shutdown_otlp();

    if !ok {
        // Try one more poll after shutdown — the shutdown hook
        // flushes the batch exporter.
        let ok2 = wait_for_spans(Duration::from_secs(5)).await;
        if ok2 {
            eprintln!("OK: collector received spans after shutdown flush");
            return;
        }
        panic!(
            "collector did not receive any spans within 20s. \
             Check that the collector is running at {endpoint} and \
             its /metrics endpoint is reachable at {}.",
            collector_metrics_url()
        );
    }
    eprintln!("OK: collector received spans");
}
