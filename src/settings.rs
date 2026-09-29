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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// HTTP server settings.
    pub server: ServerSettings,
    /// PostgreSQL durable persistence (event store + idempotency backend).
    pub postgres: PostgresSettings,
    /// Redis (cache + event bus).
    pub redis: RedisSettings,
    /// Observability (tracing, metrics).
    pub observability: ObservabilitySettings,
    /// Idempotency backend selection.
    pub idempotency: IdempotencySettings,
    /// Event bus (Redis Streams) — used by the worker binary.
    pub event_bus: EventBusSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            server: ServerSettings::default(),
            postgres: PostgresSettings::default(),
            redis: RedisSettings::default(),
            observability: ObservabilitySettings::default(),
            idempotency: IdempotencySettings::default(),
            event_bus: EventBusSettings::default(),
        }
    }
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

/// PostgreSQL settings (durable event store + idempotency backend).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PostgresSettings {
    /// `postgresql://user:pass@host:5432/dbname`
    pub dsn: String,
    /// Connection pool size.
    pub max_connections: u32,
    /// Run pending migrations on startup.
    pub run_migrations: bool,
    /// Connection acquisition timeout (seconds).
    pub acquire_timeout_secs: u64,
}

impl Default for PostgresSettings {
    fn default() -> Self {
        PostgresSettings {
            dsn: "postgresql://propfirm:propfirm@localhost:5432/propfirm".to_string(),
            max_connections: 10,
            run_migrations: true,
            acquire_timeout_secs: 5,
        }
    }
}

/// Redis settings (cache + event bus).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RedisSettings {
    /// `redis://host:6379` or `rediss://` for TLS. Comma-separated for cluster.
    pub url: String,
    /// Whether to use Redis Cluster mode.
    pub cluster: bool,
    /// Connection timeout (seconds).
    pub connect_timeout_secs: u64,
    /// Pool size per worker.
    pub pool_size: u32,
}

impl Default for RedisSettings {
    fn default() -> Self {
        RedisSettings {
            url: "redis://localhost:6379".to_string(),
            cluster: false,
            connect_timeout_secs: 3,
            pool_size: 8,
        }
    }
}

/// Observability settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ObservabilitySettings {
    /// `RUST_LOG`-style filter (`info,propfirm=debug,sqlx=warn`).
    pub log_filter: String,
    /// Log format: `json` (recommended for prod) or `pretty` (dev).
    pub log_format: String,
    /// Whether to expose the `/metrics` endpoint (Prometheus).
    pub metrics_enabled: bool,
    /// Metrics path (default `/metrics`).
    pub metrics_path: String,
    /// Whether to install a panic hook that logs panics via `tracing::error`.
    pub panic_hook: bool,
}

impl Default for ObservabilitySettings {
    fn default() -> Self {
        ObservabilitySettings {
            log_filter: "info,propfirm=debug".to_string(),
            log_format: "json".to_string(),
            metrics_enabled: true,
            metrics_path: "/metrics".to_string(),
            panic_hook: true,
        }
    }
}

/// Idempotency backend selection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IdempotencySettings {
    /// Backend: `memory` (default, dev-only), `postgres`, or `redis`.
    pub backend: String,
    /// TTL for stored idempotency keys (seconds). 0 = no TTL.
    pub ttl_secs: u64,
    /// Max entries (memory backend only).
    pub max_entries: usize,
}

impl Default for IdempotencySettings {
    fn default() -> Self {
        IdempotencySettings {
            backend: "memory".to_string(),
            ttl_secs: 24 * 60 * 60,
            max_entries: 10_000,
        }
    }
}

/// Event bus (Redis Streams) settings — used by the worker binary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EventBusSettings {
    /// Stream name for inbound evaluation requests.
    pub request_stream: String,
    /// Stream name for outbound verdicts (results).
    pub response_stream: String,
    /// Consumer group name (worker pool).
    pub consumer_group: String,
    /// Consumer name (auto-generated UUID if empty).
    pub consumer_name: String,
    /// Block timeout for XREADGROUP (milliseconds).
    pub block_ms: usize,
    /// Number of in-flight messages per worker.
    pub concurrency: usize,
    /// Idle timeout before claiming pending messages (milliseconds).
    pub idle_claim_ms: usize,
}

impl Default for EventBusSettings {
    fn default() -> Self {
        EventBusSettings {
            request_stream: "propfirm:evaluate:requests".to_string(),
            response_stream: "propfirm:evaluate:responses".to_string(),
            consumer_group: "propfirm-worker".to_string(),
            consumer_name: String::new(),
            block_ms: 5000,
            concurrency: 16,
            idle_claim_ms: 60_000,
        }
    }
}

impl Settings {
    /// Load settings from `config/propfirm.toml`, `PROPFIRM_CONFIG` path,
    /// and `PROPFIRM_*` env vars. `.env` is auto-loaded if present.
    ///
    /// # Errors
    /// Returns an error if TOML parsing fails or env var coercion fails.
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

    /// Helper: idempotency TTL as `Duration` (None if 0).
    #[must_use]
    pub fn idempotency_ttl(&self) -> Option<Duration> {
        if self.idempotency.ttl_secs == 0 {
            None
        } else {
            Some(Duration::from_secs(self.idempotency.ttl_secs))
        }
    }
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

[postgres]
dsn = "postgresql://propfirm:propfirm@localhost:5432/propfirm"
max_connections = 10
run_migrations = true
acquire_timeout_secs = 5

[redis]
url = "redis://localhost:6379"
cluster = false
connect_timeout_secs = 3
pool_size = 8

[observability]
log_filter = "info,propfirm=debug"
log_format = "json"
metrics_enabled = true
metrics_path = "/metrics"
panic_hook = true

[idempotency]
backend = "memory"
ttl_secs = 86400
max_entries = 10000

[event_bus]
request_stream = "propfirm:evaluate:requests"
response_stream = "propfirm:evaluate:responses"
consumer_group = "propfirm-worker"
block_ms = 5000
concurrency = 16
idle_claim_ms = 60000
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_load() {
        let s = Settings::default();
        assert_eq!(s.server.bind_addr, "0.0.0.0:8080");
        assert!(!s.server.tls.enabled);
        assert_eq!(s.idempotency.backend, "memory");
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
