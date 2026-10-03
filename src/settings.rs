//! Service configuration for the propfirm-engine service.
//!
//! The engine is designed to be deployed as an **internal component** of the
//! Prop Firm as a Service Platform. It receives evaluation requests either
//! from the platform backend over the private compose network (HTTP) or from
//! the centralized Redis event bus (worker). It is **never exposed publicly**
//! — TLS is for in-cluster mTLS / private-network HTTPS, and authentication
//! is intentionally absent (the caller is the platform itself).
//!
//! # Configuration sources
//!
//! Config is loaded by [`Settings::load`] in this order (later sources win):
//! 1. Inline defaults (see `DEFAULT_CONFIG_TOML` below).
//! 2. `config/propfirm.toml` if it exists in the working directory.
//! 3. `PROPFIRM_CONFIG` env var pointing to a TOML file (operator override).
//! 4. `.env` file in the working directory (loaded by `dotenvy` if present).
//! 5. Environment variables prefixed with `PROPFIRM_` (highest precedence).
//!
//! Nested keys use `__` separator, e.g. `PROPFIRM_SERVER__BIND_ADDR=0.0.0.0:9999`.
//!
//! # Example
//!
//! ```no_run
//! use propfirm::settings::Settings;
//! let settings = Settings::load().expect("config");
//! println!("bind={} tls={}", settings.server.bind_addr, settings.server.tls.enabled);
//! ```

use std::path::PathBuf;
use std::time::Duration;

use figment::{
    providers::{Env, Format, Toml},
    Figment,
};
use serde::{Deserialize, Serialize};

/// Top-level settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Settings {
    /// HTTP server settings.
    pub server: ServerSettings,
    /// Observability (tracing, metrics).
    pub observability: ObservabilitySettings,
}

/// HTTP server settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerSettings {
    /// Bind address:port, e.g. `0.0.0.0:8080`.
    pub bind_addr: String,
    /// Max request body size in bytes (default 2 MiB).
    pub max_body_bytes: usize,
    /// Per-request timeout (seconds).
    pub request_timeout_secs: u64,
    /// Graceful shutdown drain timeout (seconds).
    pub shutdown_timeout_secs: u64,
    /// TLS configuration.
    pub tls: TlsSettings,
}

impl Default for ServerSettings {
    fn default() -> Self {
        ServerSettings {
            bind_addr: "0.0.0.0:8080".to_string(),
            max_body_bytes: 2 * 1024 * 1024,
            request_timeout_secs: 30,
            shutdown_timeout_secs: 30,
            tls: TlsSettings::default(),
        }
    }
}

/// TLS termination settings.
///
/// The engine terminates TLS in-process using `rustls`. Cert/key are loaded
/// from PEM files at startup. If `enabled = false`, the server runs plain
/// HTTP — appropriate when behind an external TLS-terminating proxy.
///
/// ## mTLS (mutual TLS)
///
/// When `client_ca_path` is `Some(path)`, the server enforces mTLS: every
/// client must present a certificate signed by that CA. This is the
/// defense-in-depth pattern for service-to-service authentication on the
/// private network — even if a service token leaks, the attacker can't
/// connect without also holding a valid client cert.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TlsSettings {
    /// Whether TLS is enabled.
    pub enabled: bool,
    /// Path to the PEM-encoded certificate chain (leaf first, then intermediates).
    pub cert_path: PathBuf,
    /// Path to the PEM-encoded private key (PKCS#8 or PKCS#1).
    pub key_path: PathBuf,
    /// Optional path to a PEM-encoded CA bundle for client cert verification.
    /// When set, the server requires clients to present a certificate
    /// signed by this CA (mTLS). When `None`, mTLS is disabled.
    pub client_ca_path: Option<PathBuf>,
}

impl Default for TlsSettings {
    fn default() -> Self {
        TlsSettings {
            enabled: false,
            cert_path: PathBuf::from("/etc/propfirm/tls/cert.pem"),
            key_path: PathBuf::from("/etc/propfirm/tls/key.pem"),
            client_ca_path: None,
        }
    }
}

// D81: PostgresSettings, RedisSettings, EventBusSettings, and
// IdempotencySettings have been removed. The engine is a stateless
// compute service — no database, no Redis, no event bus, no idempotency
// backend selection. `build_state()` always returns in-memory backends.
// The platform's `workers` consumer owns all state, ordering,
// idempotency, retry, and DLQ (docs/64 §4.1).

/// Observability settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ObservabilitySettings {
    /// `RUST_LOG`-style filter (`info,propfirm=debug`).
    pub log_filter: String,
    /// Log format: `json` (recommended for prod) or `pretty` (dev).
    pub log_format: String,
    /// Whether to expose the `/metrics` endpoint (Prometheus).
    pub metrics_enabled: bool,
    /// Metrics path (default `/metrics`).
    pub metrics_path: String,
    /// Whether to install a panic hook that logs panics via `tracing::error`.
    pub panic_hook: bool,
    /// OpenTelemetry OTLP exporter settings. Only used when the `otel`
    /// cargo feature is enabled AND `otlp.endpoint` is set.
    pub otlp: OtlpSettings,
    /// When non-empty, the binary installs a tracing-flame layer
    /// that writes a flame-graph-compatible trace to this path.
    /// Requires the `flame` cargo feature. Convert to SVG with:
    ///   `flamegraph <path> > flamegraph.svg`
    pub flame_output_path: String,
}

impl Default for ObservabilitySettings {
    fn default() -> Self {
        ObservabilitySettings {
            log_filter: "info,propfirm=debug".to_string(),
            log_format: "json".to_string(),
            metrics_enabled: true,
            metrics_path: "/metrics".to_string(),
            panic_hook: true,
            otlp: OtlpSettings::default(),
            flame_output_path: String::new(),
        }
    }
}

/// OpenTelemetry OTLP exporter settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OtlpSettings {
    /// OTLP endpoint URL, e.g. `http://otel-collector:4317` (gRPC) or
    /// `http://otel-collector:4318` (HTTP). When empty, the OTLP
    /// exporter is disabled (no spans exported).
    pub endpoint: String,
    /// Protocol: `grpc` (default, recommended) or `http`.
    pub protocol: String,
    /// Service name reported to the collector.
    pub service_name: String,
    /// Whether to also export to stdout (useful for dev when no
    /// collector is available).
    pub stdout: bool,
    /// Sample ratio (0.0 to 1.0). 1.0 = sample all spans.
    pub sample_ratio: f64,
}

impl Default for OtlpSettings {
    fn default() -> Self {
        OtlpSettings {
            endpoint: String::new(),
            protocol: "grpc".to_string(),
            service_name: "propfirm-engine".to_string(),
            stdout: false,
            sample_ratio: 1.0,
        }
    }
}

// D81: IdempotencySettings and EventBusSettings removed — the engine
// is stateless, no DB, no Redis, no worker. See comment above.

impl Settings {
    /// Load settings from `config/propfirm.toml`, `PROPFIRM_CONFIG` path,
    /// and `PROPFIRM_*` env vars. `.env` is auto-loaded if present.
    ///
    /// # Errors
    /// Returns an error if TOML parsing fails or env var coercion fails.
    #[allow(clippy::result_large_err)]
    pub fn load() -> Result<Self, figment::Error> {
        // Best-effort .env load — ignores file-not-found.
        let _ = dotenvy::dotenv();

        let mut fig = Figment::from(Toml::string(DEFAULT_CONFIG_TOML));

        // Operator-provided config file (path override).
        if let Ok(path) = std::env::var("PROPFIRM_CONFIG") {
            fig = fig.merge(Toml::file(path));
        } else {
            // Try the bundled default path.
            let default_path = PathBuf::from("config/propfirm.toml");
            if default_path.exists() {
                fig = fig.merge(Toml::file(default_path));
            }
        }

        // Environment variables: PROPFIRM_SERVER__BIND_ADDR=0.0.0.0:9090
        // (double underscore = nested key)
        fig = fig.merge(Env::prefixed("PROPFIRM_").split("__"));

        fig.extract()
    }

    /// Helper: server request timeout as `Duration`.
    #[must_use]
    pub fn request_timeout(&self) -> Duration {
        Duration::from_secs(self.server.request_timeout_secs)
    }

    /// Helper: graceful shutdown drain timeout as `Duration`.
    #[must_use]
    pub fn shutdown_timeout(&self) -> Duration {
        Duration::from_secs(self.server.shutdown_timeout_secs)
    }

    // D81: idempotency_ttl() removed — the engine no longer has an
    // idempotency backend selection; it always uses in-memory.
}

/// Inline default config used as the lowest-precedence layer. The shipped
/// `config/propfirm.toml` overrides these; env vars override that.
const DEFAULT_CONFIG_TOML: &str = r#"
[server]
bind_addr = "0.0.0.0:8080"
max_body_bytes = 2097152
request_timeout_secs = 30
shutdown_timeout_secs = 30

[server.tls]
enabled = false

[observability]
log_filter = "info,propfirm=debug"
log_format = "json"
metrics_enabled = true
metrics_path = "/metrics"
panic_hook = true

[observability.otlp]
endpoint = ""
protocol = "grpc"
service_name = "propfirm-engine"
stdout = false
sample_ratio = 1.0
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_load() {
        let s = Settings::default();
        assert_eq!(s.server.bind_addr, "0.0.0.0:8080");
        assert!(!s.server.tls.enabled);
    }

    #[test]
    fn env_overrides_default() {
        // SAFETY: tests are single-threaded for env mutation when run with
        // --test-threads=1. CI sets this in the workflow.
        unsafe {
            std::env::set_var("PROPFIRM_SERVER__BIND_ADDR", "0.0.0.0:9999");
        }
        let s = Settings::load().expect("load");
        assert_eq!(s.server.bind_addr, "0.0.0.0:9999");
        unsafe {
            std::env::remove_var("PROPFIRM_SERVER__BIND_ADDR");
        }
    }
}
