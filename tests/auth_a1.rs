//! §A.1 — HTTP authentication tests.
//!
//! Proves the shipped server is no longer unauthenticated:
//! - missing credentials ⇒ 401; wrong key ⇒ 401; valid key ⇒ 200;
//! - valid key + mismatched `X-Tenant-Id` ⇒ 403;
//! - `/health` (and `/ready`) reachable without credentials;
//! - tenant is derived from the key, not the header;
//! - `/internal/*` rejects tenant keys (403) and accepts the service token;
//! - `AuthConfig::from_env` fails closed with no credentials and no
//!   escape hatch.

#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use propfirm::api::auth::{AuthConfig, AuthedIdentity};
use propfirm::api::routes::router;
use propfirm::api::server::ServerState;
use propfirm::config::presets::ftmo_phase1;
use propfirm::core::account::Account;
use propfirm::core::ids::AccountId;
use propfirm::tenant::TenantId;
use std::sync::Arc;
use tower::ServiceExt;

fn tenant() -> TenantId {
    TenantId::named("auth-test-tenant")
}

fn other_tenant() -> TenantId {
    TenantId::named("auth-test-other-tenant")
}

fn auth_config() -> AuthConfig {
    AuthConfig {
        api_keys: Arc::new(
            [
                (tenant(), "tenant-key-1".to_string()),
                (other_tenant(), "tenant-key-2".to_string()),
            ]
            .into_iter()
            .collect(),
        ),
        service_token: Some("service-token-1".to_string()),
        allow_insecure: false,
    }
}

fn make_state() -> Arc<parking_lot::RwLock<ServerState>> {
    let plan = ftmo_phase1();
    let mut account = Account::new(AccountId::new(), plan.clone())
        .with_tenant(tenant())
        .start(chrono::Utc::now())
        .unwrap();
    account.balance = propfirm::core::types::Money(propfirm::core::types::dec!(10_000));
    account.equity = account.balance;
    let state = Arc::new(parking_lot::RwLock::new(ServerState::new(
        plan,
        auth_config(),
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
) -> (StatusCode, String) {
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
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn a1_missing_credentials_rejected_401() {
    let app = router(make_state());
    let (status, _) = send_raw(
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
async fn a1_wrong_key_rejected_401() {
    let app = router(make_state());
    let (status, _) = send_raw(
        app,
        Method::GET,
        &format!("/v1/accounts/{}", AccountId::new()),
        None,
        Some("Bearer no-such-key"),
        Some(&tenant().to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a1_valid_tenant_key_accepted() {
    let state = make_state();
    // Fetch the seeded account id through the store (we know the tenant).
    let app = router(state.clone());
    // We need an actual account id; seed one explicitly here.
    let account_id;
    {
        use propfirm::persistence::traits::AccountStore;
        let acc = Account::new(AccountId::new(), ftmo_phase1())
            .with_tenant(tenant())
            .start(chrono::Utc::now())
            .unwrap();
        account_id = acc.id;
        state.read().store.put(acc).unwrap();
    }
    let (status, body) = send_raw(
        app,
        Method::GET,
        &format!("/v1/accounts/{account_id}"),
        None,
        Some("Bearer tenant-key-1"),
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "valid tenant key must authenticate; body: {body}"
    );
}

#[tokio::test]
async fn a1_tenant_derived_from_key_not_header() {
    // Key for tenant A + header claiming tenant B ⇒ 403: the header can
    // no longer select the tenant.
    let state = make_state();
    {
        use propfirm::persistence::traits::AccountStore;
        let acc = Account::new(AccountId::new(), ftmo_phase1())
            .with_tenant(tenant())
            .start(chrono::Utc::now())
            .unwrap();
        state.read().store.put(acc).unwrap();
    }
    let app = router(state);
    let (status, _) = send_raw(
        app,
        Method::GET,
        &format!("/v1/accounts/{}", AccountId::new()),
        None,
        Some("Bearer tenant-key-1"),       // tenant A's key
        Some(&other_tenant().to_string()), // claims tenant B
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn a1_health_reachable_without_credentials() {
    let app = router(make_state());
    let (status, body) = send_raw(app, Method::GET, "/health", None, None, None).await;
    assert_eq!(status, StatusCode::OK, "health must be exempt from auth");
    assert_eq!(body, "ok");
}

#[tokio::test]
async fn a1_ready_reachable_without_credentials() {
    let app = router(make_state());
    let (status, body) = send_raw(app, Method::GET, "/ready", None, None, None).await;
    assert_eq!(status, StatusCode::OK, "ready must be exempt from auth");
    assert_eq!(body, "ready");
}

#[tokio::test]
async fn a1_internal_rejects_tenant_key_403() {
    let app = router(make_state());
    let body = serde_json::json!({ "account_id": AccountId::new().to_string() }).to_string();
    let (status, _) = send_raw(
        app,
        Method::POST,
        "/internal/v1/manual-run",
        Some(body),
        Some("Bearer tenant-key-1"),
        Some(&tenant().to_string()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "/internal/* must not accept a tenant key"
    );
}

#[tokio::test]
async fn a1_internal_accepts_service_token() {
    let state = make_state();
    let account_id;
    {
        use propfirm::persistence::traits::AccountStore;
        let acc = Account::new(AccountId::new(), ftmo_phase1())
            .with_tenant(tenant())
            .start(chrono::Utc::now())
            .unwrap();
        account_id = acc.id;
        state.read().store.put(acc).unwrap();
    }
    let app = router(state);
    let body = serde_json::json!({ "account_id": account_id.to_string() }).to_string();
    let (status, resp) = send_raw(
        app,
        Method::POST,
        "/internal/v1/manual-run",
        Some(body),
        Some("Bearer service-token-1"),
        Some(&tenant().to_string()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "service token must open /internal/*; resp: {resp}"
    );
}

#[tokio::test]
async fn a1_identity_resolution_unit() {
    let cfg = auth_config();
    // Service token authenticates as Service everywhere.
    assert_eq!(
        cfg.authenticate(Some("Bearer service-token-1")),
        Some(AuthedIdentity::Service)
    );
    // Tenant key resolves to its own tenant.
    assert_eq!(
        cfg.authenticate(Some("Bearer tenant-key-2")),
        Some(AuthedIdentity::Tenant(other_tenant()))
    );
    // Unknown → None.
    assert_eq!(cfg.authenticate(Some("Bearer garbage")), None);
    // Missing/None → None.
    assert_eq!(cfg.authenticate(None), None);
}

#[test]
fn a1_from_env_fails_closed() {
    // No PROPFIRM_API_KEYS, no PROPFIRM_ALLOW_INSECURE ⇒ error.
    unsafe {
        std::env::remove_var("PROPFIRM_API_KEYS");
        std::env::remove_var("PROPFIRM_ALLOW_INSECURE");
        std::env::set_var("PROPFIRM_SERVICE_TOKEN", "svc");
    }
    assert!(
        AuthConfig::from_env().is_err(),
        "startup must fail closed when no key is configured and the escape hatch is unset"
    );
    unsafe {
        std::env::remove_var("PROPFIRM_SERVICE_TOKEN");
    }
}
