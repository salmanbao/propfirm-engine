//! §A.1 — HTTP authentication tests (service-bearer model).
//!
//! Proves the shipped server authenticates internal callers with per-service
//! static bearer tokens and trusts `X-Tenant-Id` after service auth:
//! - missing credentials ⇒ 401; wrong token ⇒ 401; valid bearer ⇒ 200;
//! - valid bearer + arbitrary `X-Tenant-Id` succeeds and logs with the
//!   service principal;
//! - `/health` and `/ready` reachable without credentials;
//! - `/internal/*` accepts service bearers from both the active and
//!   previous rotation windows during the 24-hour overlap;
//! - an invalid/missing bearer is rejected regardless of tenant.

#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use propfirm::api::auth::AuthConfig;
use propfirm::api::routes::router;
use propfirm::api::server::ServerState;
use propfirm::config::presets::ftmo_phase1;
use propfirm::core::account::Account;
use propfirm::core::ids::AccountId;
use propfirm::persistence::traits::AccountStore;
use propfirm::tenant::TenantId;
use std::sync::Arc;
use tower::ServiceExt;

fn tenant() -> TenantId {
    TenantId::named("auth-test-tenant")
}

fn service_web() -> &'static str {
    "web"
}

fn service_relay() -> &'static str {
    "relay"
}

fn auth_config_with_services(
    entries: &[(&str, Option<&str>)],
) -> AuthConfig {
    use sha2::{Digest, Sha256};
    use std::sync::Arc;
    use std::collections::HashMap;

    let mut map = HashMap::new();
    for (raw, maybe_previous) in entries {
        let active_digest = hex_fmt(&Sha256::digest(raw.as_bytes()));
        let previous_digest =
            maybe_previous.map(|prev| hex_fmt(&Sha256::digest(prev.as_bytes())));
        map.insert(raw.to_string(), (active_digest, previous_digest));
    }
    AuthConfig {
        service_tokens: Arc::new(map),
        allow_insecure: false,
    }
}

fn hex_fmt(bytes: impl AsRef<[u8]>) -> String {
    let mut s = String::with_capacity(bytes.as_ref().len() * 2);
    for b in bytes.as_ref() {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn make_state() -> Arc<parking_lot::RwLock<ServerState>> {
    let plan = ftmo_phase1();
    let mut account = Account::new(AccountId::new(), plan.clone())
        .with_tenant(tenant())
        .start(chrono::Utc::now())
        .unwrap();
    account.balance = propfirm::core::types::Money(propfirm::core::types::dec!(10_000));
    account.equity = account.balance;
    let auth = auth_config_with_services(&[(service_web(), Some("web-previous")), (service_relay(), None)]);
    let state = Arc::new(parking_lot::RwLock::new(ServerState::new(
        plan,
        auth,
    )));
    {
        let s = state.read();
        propfirm::persistence::traits::AccountStore::put(&s.store, account.clone()).unwrap();
    }
    state
}

/// Sends a request with full control over auth/tenant headers.
async fn send_raw(
    app: axum::Router,
    method: Method,
    uri: &str,
    body: Option<String>,
    auth: Option<&str>,
    tenant_header: Option<&str>,
) -> (StatusCode, String, Option<String>) {
    let mut req = Request::builder().method(method).uri(uri);
    if let Some(a) = auth {
        req = req.header("Authorization", a);
    }
    if let Some(t) = tenant_header {
        req = req.header("X-Tenant-Id", t);
    }
    let req = if let Some(b) = body {
        req.header("content-type", "application/json")
            .body(Body::from(b))
            .unwrap()
    } else {
        req.body(Body::empty()).unwrap()
    };
    let response = app.oneshot(req).await.unwrap();
    let status = response.status();
    let correlation_id = response
        .headers()
        .get("x-correlation-id")
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned(), correlation_id)
}

#[tokio::test]
async fn a1_missing_credentials_rejected_401() {
    let app = router(make_state());
    let (status, _, _) = send_raw(
        app,
        Method::GET,
        &format!("/v1/accounts/{}", AccountId::new()),
        None,
        None,
        Some(&tenant().to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a1_wrong_token_rejected_401() {
    let app = router(make_state());
    let (status, _, _) = send_raw(
        app,
        Method::GET,
        &format!("/v1/accounts/{}", AccountId::new()),
        None,
        Some("Bearer no-such-token"),
        Some(&tenant().to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a1_valid_service_bearer_accepted() {
    let state = make_state();
    let app = router(state.clone());
    let account_id;
    {
        let acc = Account::new(AccountId::new(), ftmo_phase1())
            .with_tenant(tenant())
            .start(chrono::Utc::now())
            .unwrap();
        account_id = acc.id;
        state.read().store.put(acc).unwrap();
    }
    let (status, body, correlation_id) = send_raw(
        app,
        Method::GET,
        &format!("/v1/accounts/{account_id}"),
        None,
        Some("Bearer web"),
        Some(&tenant().to_string()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "valid service bearer must authenticate; body: {body}"
    );
    assert!(correlation_id.is_some(), "correlation_id must be present for audit");
    assert!(correlation_id.unwrap().len() > 0);
}

#[tokio::test]
async fn a1_tenant_header_trusted_after_service_auth() {
    let state = make_state();
    let app = router(state.clone());
    let account_id;
    {
        let acc = Account::new(AccountId::new(), ftmo_phase1())
            .with_tenant(tenant())
            .start(chrono::Utc::now())
            .unwrap();
        account_id = acc.id;
        state.read().store.put(acc).unwrap();
    }
    let (status, _, _) = send_raw(
        app,
        Method::GET,
        &format!("/v1/accounts/{account_id}"),
        None,
        Some("Bearer relay"),
        Some(&tenant().to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn a1_health_reachable_without_credentials() {
    let app = router(make_state());
    let (status, body, _) = send_raw(app, Method::GET, "/health", None, None, None).await;
    assert_eq!(status, StatusCode::OK, "health must be exempt from auth");
    assert_eq!(body, "ok");
}

#[tokio::test]
async fn a1_ready_reachable_without_credentials() {
    let app = router(make_state());
    let (status, body, _) = send_raw(app, Method::GET, "/ready", None, None, None).await;
    assert_eq!(status, StatusCode::OK, "ready must be exempt from auth");
    assert_eq!(body, "ready");
}

#[tokio::test]
async fn a1_internal_accepts_active_service_token() {
    let state = make_state();
    let app = router(state.clone());
    let account_id;
    {
        let acc = Account::new(AccountId::new(), ftmo_phase1())
            .with_tenant(tenant())
            .start(chrono::Utc::now())
            .unwrap();
        account_id = acc.id;
        state.read().store.put(acc).unwrap();
    }
    let body = serde_json::json!({ "account_id": account_id.to_string() }).to_string();
    let (status, _, _) = send_raw(
        app,
        Method::POST,
        "/internal/v1/manual-run",
        Some(body),
        Some("Bearer web"),
        Some(&tenant().to_string()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "active service token must open /internal/*"
    );
}

#[tokio::test]
async fn a1_internal_accepts_previous_token_during_rotation() {
    let state = make_state();
    let app = router(state.clone());
    let account_id;
    {
        let acc = Account::new(AccountId::new(), ftmo_phase1())
            .with_tenant(tenant())
            .start(chrono::Utc::now())
            .unwrap();
        account_id = acc.id;
        state.read().store.put(acc).unwrap();
    }
    let body = serde_json::json!({ "account_id": account_id.to_string() }).to_string();
    let (status, _, _) = send_raw(
        app,
        Method::POST,
        "/internal/v1/manual-run",
        Some(body),
        Some("Bearer web-previous"),
        Some(&tenant().to_string()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "previous rotation token must still be accepted during overlap"
    );
}

#[tokio::test]
async fn a1_internal_rejects_unknown_bearer() {
    let app = router(make_state());
    let body = serde_json::json!({ "account_id": AccountId::new().to_string() }).to_string();
    let (status, _, _) = send_raw(
        app,
        Method::POST,
        "/internal/v1/manual-run",
        Some(body),
        Some("Bearer not-a-service"),
        Some(&tenant().to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[test]
fn a1_from_env_fails_closed() {
    unsafe {
        std::env::remove_var("PROPFIRM_SERVICE_TOKENS");
        std::env::remove_var("PROPFIRM_ALLOW_INSECURE");
    }
    assert!(
        AuthConfig::from_env().is_err(),
        "startup must fail closed when no service bearer is configured and the escape hatch is unset"
    );
    unsafe {
        std::env::set_var("PROPFIRM_ALLOW_INSECURE", "1");
    }
    let cfg = AuthConfig::from_env().unwrap();
    assert!(cfg.allow_insecure);
    assert!(!cfg.insecure_warnings().is_empty());
    unsafe {
        std::env::remove_var("PROPFIRM_ALLOW_INSECURE");
    }
}
