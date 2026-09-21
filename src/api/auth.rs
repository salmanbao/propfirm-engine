//! Authentication and authorization (§A.1 fix).
//!
//! The previously shipped server was **unauthenticated**: `ServerState.api_key`
//! was initialized to `None`, never set from any config source, and the
//! middleware short-circuited on `None` — scaffolding that looked protected
//! but wasn't. This module replaces that with:
//!
//! - **Fail-closed startup**: the server refuses to start without
//!   credentials unless `PROPFIRM_ALLOW_INSECURE=1` is set explicitly
//!   (with a loud warning).
//! - **Per-tenant keys**: each tenant has its own key. The tenant is
//!   derived from the authenticated credential — the `X-Tenant-Id`
//!   header, when present, must *match* the key's tenant (403 on
//!   mismatch) so a caller can never read another tenant's data by
//!   header tampering.
//! - **Separate service token for `/internal/*`**: bridge/platform-only
//!   endpoints reject tenant keys and accept only the service token.
//! - **Constant-time comparison** for every secret.
//! - **Exemptions**: `/health` and `/ready` are unauthenticated so
//!   readiness probes work.

use crate::tenant::TenantId;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use std::collections::HashMap;
use std::sync::Arc;
use subtle::ConstantTimeEq;
use uuid::Uuid;

/// Where credentials are read from at startup.
pub const ENV_API_KEYS: &str = "PROPFIRM_API_KEYS";
/// Escape hatch that permits an unauthenticated server (deliberate,
/// explicit, loud).
pub const ENV_ALLOW_INSECURE: &str = "PROPFIRM_ALLOW_INSECURE";
/// Environment variable holding the `/internal/*` service token.
pub const ENV_SERVICE_TOKEN: &str = "PROPFIRM_SERVICE_TOKEN";
/// Escape hatch for the service token (dev/test only).
pub const ENV_ALLOW_NO_SERVICE_TOKEN: &str = "PROPFIRM_ALLOW_NO_SERVICE_TOKEN";

/// Header used to authenticate.
pub const AUTH_HEADER: &str = "authorization";
/// Legacy header: must still *match* the authenticated tenant when
/// present, but can no longer *select* the tenant.
pub const TENANT_HEADER: &str = "x-tenant-id";

/// Authenticated identity resolved from the request's credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthedIdentity {
    /// A tenant key holder. Carries the tenant the key belongs to.
    Tenant(TenantId),
    /// The platform service (bridge / LCC). May act on any tenant.
    Service,
}

/// Environment parsing for [`AuthConfig`].
///
/// `PROPFIRM_API_KEYS` format: comma-separated `tenant-uuid:key` pairs,
/// e.g. `123e4567-...:secret-a,9af0c3f2-...:secret-b`.
fn parse_api_keys(spec: &str) -> Result<HashMap<TenantId, String>, crate::core::Error> {
    let mut map = HashMap::new();
    for pair in spec.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let Some((tenant_str, key)) = pair.split_once(':') else {
            return Err(crate::core::Error::invalid_config(format!(
                "{ENV_API_KEYS}: entry '{pair}' is not `tenant-uuid:key`"
            )));
        };
        let tenant_uuid = Uuid::parse_str(tenant_str.trim()).map_err(|e| {
            crate::core::Error::invalid_config(format!(
                "{ENV_API_KEYS}: tenant id '{tenant_str}' is not a UUID: {e}"
            ))
        })?;
        let key = key.trim();
        if key.is_empty() {
            return Err(crate::core::Error::invalid_config(format!(
                "{ENV_API_KEYS}: empty key for tenant {tenant_str}"
            )));
        }
        map.insert(TenantId::from_uuid(tenant_uuid), key.to_string());
    }
    if map.is_empty() {
        return Err(crate::core::Error::invalid_config(format!(
            "{ENV_API_KEYS}: no `tenant-uuid:key` entries found"
        )));
    }
    Ok(map)
}

/// Resolved auth configuration.
#[derive(Clone)]
pub struct AuthConfig {
    /// Tenant id → API key.
    pub api_keys: Arc<HashMap<TenantId, String>>,
    /// The `/internal/*` service token.
    pub service_token: Option<String>,
    /// Whether the server was started deliberately unauthenticated.
    pub allow_insecure: bool,
}

impl std::fmt::Debug for AuthConfig {
    /// Deliberately does not print any key material.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthConfig")
            .field("api_keys", &format!("<{} keys>", self.api_keys.len()))
            .field(
                "service_token",
                &self.service_token.as_ref().map(|_| "<redacted>"),
            )
            .field("allow_insecure", &self.allow_insecure)
            .finish()
    }
}

impl AuthConfig {
    /// Reads the configuration from the environment.
    ///
    /// Fails closed: returns an error unless at least one tenant key is
    /// configured (or `PROPFIRM_ALLOW_INSECURE=1` is set explicitly).
    pub fn from_env() -> Result<Self, crate::core::Error> {
        let allow_insecure = std::env::var(ENV_ALLOW_INSECURE)
            .map(|v| v == "1")
            .unwrap_or(false);
        let api_keys = match std::env::var(ENV_API_KEYS) {
            Ok(spec) => Some(Arc::new(parse_api_keys(&spec)?)),
            Err(_) => None,
        };
        let service_token = std::env::var(ENV_SERVICE_TOKEN)
            .ok()
            .filter(|t| !t.is_empty());
        if api_keys.is_none() && !allow_insecure {
            return Err(crate::core::Error::invalid_config(format!(
                "refusing to start unauthenticated: set {ENV_API_KEYS} \
                 (comma-separated `tenant-uuid:key` pairs) or explicitly set \
                 {ENV_ALLOW_INSECURE}=1 to run without auth (NOT for production)"
            )));
        }
        // /internal/* is bridge-only; require a distinct service token
        // unless the operator explicitly opts out.
        if service_token.is_none()
            && api_keys.is_some()
            && !std::env::var(ENV_ALLOW_NO_SERVICE_TOKEN)
                .map(|v| v == "1")
                .unwrap_or(false)
        {
            return Err(crate::core::Error::invalid_config(format!(
                "refusing to start without a service token: set {ENV_SERVICE_TOKEN} \
                 (the /internal/* endpoints are bridge/platform-only and must not \
                 accept tenant keys) or explicitly set {ENV_ALLOW_NO_SERVICE_TOKEN}=1"
            )));
        }
        Ok(AuthConfig {
            api_keys: api_keys.unwrap_or_default(),
            service_token,
            allow_insecure,
        })
    }

    /// Returns the loud startup warning lines for insecure configurations.
    #[must_use]
    pub fn insecure_warnings(&self) -> Vec<String> {
        let mut w = Vec::new();
        if self.allow_insecure && self.api_keys.is_empty() {
            w.push(format!(
                "WARNING: {ENV_ALLOW_INSECURE}=1 — this server is running WITHOUT authentication. NOT for production."
            ));
        }
        if self.api_keys.is_empty() {
            w.push("WARNING: no tenant API keys configured — every /v1 request will be rejected with 401.".to_string());
        }
        if self.service_token.is_none() {
            w.push(format!(
                "WARNING: no {ENV_SERVICE_TOKEN} configured — every /internal/* request will be rejected with 401."
            ));
        }
        w
    }

    /// Constant-time check of a presented key against the expected key.
    fn key_matches(presented: &str, expected: &str) -> bool {
        // Constant-time comparison; pad to equal length so length is not
        // leaked either (the pad here is not secret — both keys are
        // server-side data — but equal-length ct_eq is the safe default).
        let mut a = presented.as_bytes().to_vec();
        let mut b = expected.as_bytes().to_vec();
        let n = a.len().max(b.len());
        a.resize(n, 0);
        b.resize(n, 0);
        a.as_slice().ct_eq(b.as_slice()).into()
    }

    /// Resolves the identity from the `Authorization: Bearer <key>` header.
    #[must_use]
    pub fn authenticate(&self, auth_header: Option<&str>) -> Option<AuthedIdentity> {
        let raw = auth_header?;
        let key = raw
            .strip_prefix("Bearer ")
            .or_else(|| raw.strip_prefix("bearer "))?;
        if key.is_empty() {
            return None;
        }
        // Service token first (it authorizes the platform role).
        if let Some(expected) = &self.service_token {
            if Self::key_matches(key, expected) {
                return Some(AuthedIdentity::Service);
            }
        }
        // Tenant keys.
        for (tenant, expected) in self.api_keys.iter() {
            if Self::key_matches(key, expected) {
                return Some(AuthedIdentity::Tenant(*tenant));
            }
        }
        None
    }

    /// Paths that never require credentials (readiness probes).
    #[must_use]
    fn is_exempt(path: &str) -> bool {
        path == "/health" || path == "/ready"
    }

    /// Paths under `/internal/` require the service identity.
    #[must_use]
    fn is_internal(path: &str) -> bool {
        path.starts_with("/internal/") || path == "/internal"
    }
}

/// The axum middleware. Resolves the identity, enforces per-route
/// requirements and injects [`AuthedIdentity`] for the handlers.
///
/// Semantics:
/// - `/health`, `/ready`: exempt.
/// - `/internal/*`: require `AuthedIdentity::Service` (tenant keys are
///   rejected with 403 — those endpoints are bridge/platform-only).
/// - everything else: any valid identity; tenant endpoints resolve the
///   tenant from the key. A presented `X-Tenant-Id` that mismatches the
///   authenticated tenant is a 403.
pub async fn auth_layer(
    req: Request<axum::body::Body>,
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
    if AuthConfig::is_internal(&path) && identity != AuthedIdentity::Service {
        return Err(StatusCode::FORBIDDEN);
    }
    // Tenant header cross-check (P1-9 defence in depth): if the caller
    // presents X-Tenant-Id it must agree with the authenticated tenant.
    if let (AuthedIdentity::Tenant(tenant), Some(presented)) = (
        &identity,
        req.headers()
            .get(TENANT_HEADER)
            .and_then(|v| v.to_str().ok()),
    ) {
        let presented = presented.trim();
        let presented_uuid = Uuid::parse_str(presented).map_err(|_| StatusCode::BAD_REQUEST)?;
        if TenantId::from_uuid(presented_uuid) != *tenant {
            return Err(StatusCode::FORBIDDEN);
        }
    }
    let mut req = req;
    req.extensions_mut().insert(identity);
    Ok(next.run(req).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with_keys(entries: &[(TenantId, &str)], service: Option<&str>) -> AuthConfig {
        AuthConfig {
            api_keys: Arc::new(entries.iter().map(|(t, k)| (*t, k.to_string())).collect()),
            service_token: service.map(std::string::ToString::to_string),
            allow_insecure: false,
        }
    }

    #[test]
    fn parse_api_keys_roundtrip() {
        let t1 = TenantId::named("alpha");
        let t2 = TenantId::named("beta");
        let spec = format!("{t1}:key-a, {t2}:key-b");
        let map = parse_api_keys(&spec).unwrap();
        assert_eq!(map.get(&t1).map(String::as_str), Some("key-a"));
        assert_eq!(map.get(&t2).map(String::as_str), Some("key-b"));
    }

    #[test]
    fn parse_api_keys_rejects_garbage() {
        assert!(parse_api_keys("no-colon-here").is_err());
        assert!(parse_api_keys("not-a-uuid:key").is_err());
        assert!(parse_api_keys("").is_err());
    }

    #[test]
    fn authenticate_missing_or_wrong_key_is_none() {
        let t = TenantId::named("t");
        let cfg = config_with_keys(&[(t, "sekrit")], Some("svc"));
        assert!(cfg.authenticate(None).is_none());
        assert!(cfg.authenticate(Some("Basic abc")).is_none());
        assert!(cfg.authenticate(Some("Bearer wrong")).is_none());
        assert!(cfg.authenticate(Some("Bearer ")).is_none());
    }

    #[test]
    fn authenticate_service_and_tenant_keys() {
        let t = TenantId::named("t");
        let cfg = config_with_keys(&[(t, "tenant-key")], Some("svc-token"));
        assert_eq!(
            cfg.authenticate(Some("Bearer svc-token")),
            Some(AuthedIdentity::Service)
        );
        assert_eq!(
            cfg.authenticate(Some("Bearer tenant-key")),
            Some(AuthedIdentity::Tenant(t))
        );
    }

    #[test]
    fn constant_time_compare_does_not_shortcircuit() {
        // Not a cryptographic proof — a smoke check that equal-length and
        // different-length comparisons both return the right answer.
        assert!(AuthConfig::key_matches("abc", "abc"));
        assert!(!AuthConfig::key_matches("abc", "abd"));
        assert!(!AuthConfig::key_matches("abc", "abcd"));
    }

    #[test]
    fn exempt_and_internal_paths() {
        assert!(AuthConfig::is_exempt("/health"));
        assert!(AuthConfig::is_exempt("/ready"));
        assert!(!AuthConfig::is_exempt("/v1/accounts"));
        assert!(AuthConfig::is_internal("/internal/v1/evaluate"));
        assert!(!AuthConfig::is_internal("/v1/evaluate-order"));
    }

    #[test]
    fn from_env_fails_closed_without_keys() {
        // Temporarily set/clear the env vars around the call.
        // SAFETY: single-threaded test binary per crate; env races are
        // not a concern for these unit tests.
        unsafe {
            std::env::remove_var(ENV_API_KEYS);
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
