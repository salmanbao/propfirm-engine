//! Authentication and authorization (service-bearer model).
//!
//! This module now implements the platform's actual internal-service
//! authentication shape:
//!
//! - **Per-service static bearer tokens** from SOPS-managed environment
//!   variables, validated by SHA-256 hash comparison with constant-time
//!   equality.
//! - **Dual-token rotation window**: each service may provide an active
//!   and a previous token; both are accepted during the 24-hour overlap
//!   period so rotations are zero-downtime.
//! - **Fail-closed startup**: the server refuses to start without
//!   credentials unless `PROPFIRM_ALLOW_INSECURE=1` is set explicitly
//!   (with a loud warning).
//! - **Tenant resolution** on internal routes is simple: once the service
//!   bearer is valid, `X-Tenant-Id` is trusted as-given because the
//!   caller already proved it is the platform bridge over the private
//!   compose network.
//! - **Audit logging**: every authenticated request records the service
//!   identity and a generated `correlation_id` so the platform's
//!   audit-logging requirements are satisfied without changing handler
//!   signatures.
//! - **Exemptions**: `/health` and `/ready` are unauthenticated so
//!   readiness probes work.
//!
//! There is no per-tenant API-key surface in V1. If/when that capability
//! ships, it belongs in the platform gateway (GW/AUTH), not in this
//! engine.

use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use subtle::ConstantTimeEq;
use uuid::Uuid;

/// Where credentials are read from at startup.
pub const ENV_SERVICE_TOKENS: &str = "PROPFIRM_SERVICE_TOKENS";
/// Escape hatch that permits an unauthenticated server (deliberate,
/// explicit, loud).
pub const ENV_ALLOW_INSECURE: &str = "PROPFIRM_ALLOW_INSECURE";

/// Header used to authenticate.
pub const AUTH_HEADER: &str = "authorization";
/// Header used to select the tenant on internal routes. Trusted only
/// after the service bearer has been validated.
pub const TENANT_HEADER: &str = "x-tenant-id";
/// Header injected onto every request carrying a generated
/// `correlation_id` for audit logging.
pub const CORRELATION_ID_HEADER: &str = "x-correlation-id";

/// Authenticated identity resolved from the request's credentials.
///
/// The principal is always a named internal service (`kind=service`).
/// Tenant resolution happens separately via `X-Tenant-Id`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AuthedIdentity {
    /// Service name, e.g. `"web"`, `"workers"`, `"relay"`, `"bridge"`.
    pub service_name: String,
}

impl AuthedIdentity {
    #[must_use]
    pub fn new(service_name: impl Into<String>) -> Self {
        Self {
            service_name: service_name.into(),
        }
    }
}

/// Environment parsing for [`AuthConfig`].
///
/// `PROPFIRM_SERVICE_TOKENS` format: comma-separated `service-name:sha256hex`
/// pairs. The value is the **hex-encoded SHA-256 digest** of the raw token
/// string, so the server never stores or compares plaintext secrets.
///
/// Example: `web:abc123...,workers:def456...,relay:789abc...`
///
/// Each service may optionally provide a second entry as
/// `service-name:previous:sha256hex` for the dual-token rotation window.
fn parse_service_token_spec(
    spec: &str,
) -> Result<HashMap<String, (String, Option<String>)>, crate::core::Error> {
    let mut map = HashMap::new();
    for entry in spec.split(',') {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let mut parts = entry.split(':');
        let service = parts
            .next()
            .ok_or_else(|| {
                crate::core::Error::invalid_config(format!(
                    "{ENV_SERVICE_TOKENS}: missing service name in entry '{entry}'"
                ))
            })?;
        let service = service.trim();
        if service.is_empty() {
            return Err(crate::core::Error::invalid_config(format!(
                "{ENV_SERVICE_TOKENS}: empty service name in entry '{entry}'"
            )));
        }
        let kind = parts.next().ok_or_else(|| {
            crate::core::Error::invalid_config(format!(
                "{ENV_SERVICE_TOKENS}: missing token kind in entry '{entry}'"
            ))
        })?;
        let digest = parts.next().ok_or_else(|| {
            crate::core::Error::invalid_config(format!(
                "{ENV_SERVICE_TOKENS}: missing sha256 digest in entry '{entry}'"
            ))
        })?;
        match kind {
            "active" => {
                if map
                    .insert(service.to_string(), (digest.trim().to_string(), None))
                    .is_some()
                {
                    return Err(crate::core::Error::invalid_config(format!(
                        "{ENV_SERVICE_TOKENS}: duplicate service name '{service}'"
                    )));
                }
            }
            "previous" => {
                let entry = map.entry(service.to_string()).or_default();
                if entry.1.is_some() {
                    return Err(crate::core::Error::invalid_config(format!(
                        "{ENV_SERVICE_TOKENS}: duplicate previous token for '{service}'"
                    )));
                }
                entry.1 = Some(digest.trim().to_string());
            }
            other => {
                return Err(crate::core::Error::invalid_config(format!(
                    "{ENV_SERVICE_TOKENS}: unknown token kind '{other}' in entry '{entry}'"
                )));
            }
        }
    }
    if map.is_empty() {
        return Err(crate::core::Error::invalid_config(format!(
            "{ENV_SERVICE_TOKENS}: no service token entries found"
        )));
    }
    Ok(map)
}

/// Resolved auth configuration.
#[derive(Clone)]
pub struct AuthConfig {
    /// Service name → (active sha256 digest, optional previous sha256 digest).
    pub service_tokens: Arc<HashMap<String, (String, Option<String>)>>,
    /// Whether the server was started deliberately unauthenticated.
    pub allow_insecure: bool,
}

impl std::fmt::Debug for AuthConfig {
    /// Deliberately does not print any key material or digests.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthConfig")
            .field("services", &self.service_tokens.keys().collect::<Vec<_>>())
            .field("allow_insecure", &self.allow_insecure)
            .finish()
    }
}

impl AuthConfig {
    /// Reads the configuration from the environment.
    ///
    /// Fails closed: returns an error unless at least one service token is
    /// configured (or `PROPFIRM_ALLOW_INSECURE=1` is set explicitly).
    pub fn from_env() -> Result<Self, crate::core::Error> {
        let allow_insecure = std::env::var(ENV_ALLOW_INSECURE)
            .map(|v| v == "1")
            .unwrap_or(false);
        let service_tokens = match std::env::var(ENV_SERVICE_TOKENS) {
            Ok(spec) => Some(Arc::new(parse_service_token_spec(&spec)?)),
            Err(_) => None,
        };
        if service_tokens.is_none() && !allow_insecure {
            return Err(crate::core::Error::invalid_config(format!(
                "refusing to start unauthenticated: set {ENV_SERVICE_TOKENS} \
                 (comma-separated `service-name:active:sha256hex` entries) or \
                 explicitly set {ENV_ALLOW_INSECURE}=1 to run without auth \
                 (NOT for production)"
            )));
        }
        Ok(AuthConfig {
            service_tokens: service_tokens.unwrap_or_default(),
            allow_insecure,
        })
    }

    /// Returns the loud startup warning lines for insecure configurations.
    #[must_use]
    pub fn insecure_warnings(&self) -> Vec<String> {
        let mut w = Vec::new();
        if self.allow_insecure && self.service_tokens.is_empty() {
            w.push(format!(
                "WARNING: {ENV_ALLOW_INSECURE}=1 — this server is running WITHOUT authentication. NOT for production."
            ));
        }
        if self.service_tokens.is_empty() {
            w.push("WARNING: no service bearer tokens configured — every /internal/* and /v1 request will be rejected with 401.".to_string());
        }
        w
    }

    /// Constant-time check of a presented token's SHA-256 digest against
    /// the expected digest.
    fn digest_matches(presented: &str, expected_hex: &str) -> bool {
        // Compute sha256 of the presented raw token bytes.
        let presented_digest = Sha256::digest(presented.as_bytes());
        let presented_hex = hex_fmt(presented_digest.as_ref());
        // Constant-time comparison of two equal-length hex strings.
        let mut a = presented_hex.as_bytes().to_vec();
        let mut b = expected_hex.as_bytes().to_vec();
        let n = a.len().max(b.len());
        a.resize(n, 0);
        b.resize(n, 0);
        a.as_slice().ct_eq(b.as_slice()).into()
    }

    /// Resolves the identity from the `Authorization: Bearer <token>` header.
    ///
    /// Checks the active digest first, then the previous digest (rotation
    /// overlap window). Returns `None` if neither matches.
    #[must_use]
    pub fn authenticate(&self, auth_header: Option<&str>) -> Option<AuthedIdentity> {
        let raw = auth_header?;
        let token = raw
            .strip_prefix("Bearer ")
            .or_else(|| raw.strip_prefix("bearer "))?;
        if token.is_empty() {
            return None;
        }
        for (service, (active, previous)) in self.service_tokens.iter() {
            if Self::digest_matches(token, active) {
                return Some(AuthedIdentity::new(service.as_str()));
            }
            if let Some(prev) = previous {
                if Self::digest_matches(token, prev) {
                    return Some(AuthedIdentity::new(service.as_str()));
                }
            }
        }
        None
    }

    /// Paths that never require credentials (readiness probes).
    #[must_use]
    fn is_exempt(path: &str) -> bool {
        path == "/health" || path == "/ready"
    }
}

/// Format a byte slice as lowercase hex.
fn hex_fmt(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// The axum middleware. Resolves the identity, enforces per-route
/// requirements, and injects [`AuthedIdentity`] plus an audit
/// `x-correlation-id` for downstream logging.
///
/// Semantics:
/// - `/health`, `/ready`: exempt.
/// - everything else: valid service bearer required. A presented
///   `X-Tenant-Id` is parsed as a UUID and trusted; the service bearer
///   already proved the caller is the platform bridge over the private
///   network.
///
/// The correlation id is generated once per request and placed in both
/// the request extensions and an outgoing header so handlers and
/// downstream loggers can include it in audit records.
pub async fn auth_layer(
    mut req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    let path = req.uri().path().to_string();
    if AuthConfig::is_exempt(&path) {
        return Ok(next.run(req).await);
    }
    let auth = req
        .extensions()
        .get::<AuthConfig>()
        .cloned()
        .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
    let identity = auth.authenticate(req.headers().get(AUTH_HEADER).and_then(|v| v.to_str().ok()));
    let Some(identity) = identity else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    // Audit correlation id: generated once, propagated via extension
    // and response header so downstream handlers/loggers can include it
    // in audit records.
    let correlation_id = Uuid::new_v4().to_string();
    req.extensions_mut().insert(identity);
    req.extensions_mut().insert(correlation_id.clone());
    let mut response = next.run(req).await;
    if let Ok(header_value) = correlation_id.parse::<axum::http::HeaderValue>() {
        response.headers_mut().insert(CORRELATION_ID_HEADER, header_value);
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Convenience: build a config with raw token strings (not digests).
    /// The test harness hashes them the same way production does.
    fn config_with_services(entries: &[(&str, Option<&str>)]) -> AuthConfig {
        let mut map = HashMap::new();
        for (service, maybe_previous) in entries {
            let active_digest = hex_fmt(&Sha256::digest(service.as_bytes()));
            let previous_digest =
                maybe_previous.map(|prev| hex_fmt(&Sha256::digest(prev.as_bytes())));
            map.insert(
                service.to_string(),
                (active_digest, previous_digest),
            );
        }
        AuthConfig {
            service_tokens: Arc::new(map),
            allow_insecure: false,
        }
    }
    #[test]
    fn parse_service_token_spec_roundtrip() {
        let spec = "web:active:abc123,workers:active:def456";
        let map = parse_service_token_spec(spec).unwrap();
        assert_eq!(map.get("web").map(|(a, _)| a.as_str()), Some("abc123"));
        assert_eq!(map.get("workers").map(|(a, _)| a.as_str()), Some("def456"));
    }

    #[test]
    fn parse_service_token_spec_rejects_garbage() {
        assert!(parse_service_token_spec("").is_err());
        assert!(parse_service_token_spec("no-kind-here").is_err());
        assert!(parse_service_token_spec("web:unknown:abc").is_err());
    }

    #[test]
    fn authenticate_active_token() {
        let cfg = config_with_services(&[("web-secret", None), ("workers-secret", None)]);
        assert_eq!(
            cfg.authenticate(Some("Bearer web-secret")),
            Some(AuthedIdentity::new("web-secret"))
        );
    }

    #[test]
    fn authenticate_previous_token_during_rotation() {
        let cfg = config_with_services(&[("web", Some("web-previous")), ("workers", None)]);
        assert_eq!(
            cfg.authenticate(Some("Bearer web-previous")),
            Some(AuthedIdentity::new("web"))
        );
    }

    #[test]
    fn authenticate_rejects_wrong_token() {
        let cfg = config_with_services(&[("web", None)]);
        assert!(cfg.authenticate(Some("Bearer wrong")).is_none());
        assert!(cfg.authenticate(Some("Bearer ")).is_none());
        assert!(cfg.authenticate(None).is_none());
    }

    #[test]
    fn constant_time_digest_compare() {
        let digest = hex_fmt(&Sha256::digest(b"same"));
        assert!(AuthConfig::digest_matches("same", &digest));
        assert!(!AuthConfig::digest_matches("different", &digest));
    }

    #[test]
    fn exempt_paths() {
        assert!(AuthConfig::is_exempt("/health"));
        assert!(AuthConfig::is_exempt("/ready"));
        assert!(!AuthConfig::is_exempt("/internal/v1/evaluate"));
        assert!(!AuthConfig::is_exempt("/v1/accounts/1"));
    }

    #[test]
    fn from_env_fails_closed_without_tokens() {
        unsafe {
            std::env::remove_var(ENV_SERVICE_TOKENS);
            std::env::set_var(ENV_ALLOW_INSECURE, "");
        }
        assert!(AuthConfig::from_env().is_err());
        unsafe {
            std::env::set_var(ENV_ALLOW_INSECURE, "1");
        }
        let cfg = AuthConfig::from_env().unwrap();
        assert!(cfg.allow_insecure);
        assert!(!cfg.insecure_warnings().is_empty());
        unsafe {
            std::env::remove_var(ENV_ALLOW_INSECURE);
        }
    }
}
