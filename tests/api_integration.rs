//! P0-F: API integration tests — prove the HTTP server actually works
//! end-to-end (P0-A fix verification).
//!
//! Before P0-A, `ServerState::clone` re-instantiated empty
//! `InMemoryStore`/`EventStore`/`LogNotifier` on every clone, so every
//! `state.read().clone()` in a handler discarded all server state.
//! Every endpoint except `/health` returned 404. There were **zero**
//! API tests — that's how the broken server shipped.
//!
//! These tests use `axum::http::Request` + `tower::ServiceExt::oneshot`
//! to exercise each endpoint against the same `ServerState` instance,
//! proving:
//! - `/health` works
//! - `POST /v1/evaluate-order` returns a verdict (not "account not found")
//! - `POST /internal/v1/evaluate` returns a stateless verdict with input_hash
//! - `POST /internal/v1/manual-run` returns a decision
//! - `GET /internal/v1/breach-report/:account_id` returns the breach log
//!
//! ADR-11 removed account CRUD (`GET/POST /v1/accounts`) and `AccountStore`:
//! the server holds no account state, so there is no seeded-account
//! lookup to test here.

#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt; // for collect()
use propfirm::api::auth::AuthConfig;
use propfirm::api::routes::router;
use propfirm::api::server::ServerState;
use propfirm::config::presets::ftmo_phase1;
use propfirm::core::account::Account;
use propfirm::core::ids::AccountId;

use propfirm::tenant::TenantId;
use std::sync::Arc;
use tower::ServiceExt;

fn test_tenant_id() -> TenantId {
    TenantId::named("test-tenant")
}

fn test_tenant_id_str() -> String {
    test_tenant_id().to_string()
}

fn hex_fmt(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// **§A.1 fix**: the test config now carries a per-service bearer token
/// digest. The raw token `service-secret` authenticates `/internal/*`
/// and `/v1/*`; `X-Tenant-Id` selects the tenant after service auth.
fn test_auth_config() -> AuthConfig {
    use sha2::{Digest, Sha256};
    let active = hex_fmt(Sha256::digest(SERVICE_KEY.as_bytes()));
    AuthConfig {
        service_tokens: std::sync::Arc::new(
            [(SERVICE_KEY.to_string(), (active, None))]
                .into_iter()
                .collect(),
        ),
        allow_insecure: false,
    }
}

const SERVICE_KEY: &str = "service-secret";

/// Builds a `ServerState` plus a matching in-memory `Account`.
///
/// The account is returned so tests can serialize it into
/// `account_state` — ADR-11: the server itself holds no account state.
async fn make_state_with_account() -> (Arc<tokio::sync::RwLock<ServerState>>, Account) {
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan.clone())
        .with_tenant(test_tenant_id())
        .start(chrono::Utc::now())
        .unwrap();
    let state = Arc::new(tokio::sync::RwLock::new(ServerState::new(
        test_auth_config(),
    )));
    (state, account)
}

/// Helper: send a request and return (status, body_text).
async fn send(
    app: axum::Router,
    method: Method,
    uri: &str,
    body: Option<String>,
    tenant_id: &str,
) -> (StatusCode, String) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("X-Tenant-Id", tenant_id)
        .header("Authorization", format!("Bearer {SERVICE_KEY}"));
    let req = if let Some(b) = body {
        req.header("content-type", "application/json")
            .body(Body::from(b))
            .unwrap()
    } else {
        req.body(Body::empty()).unwrap()
    };
    let response = app.oneshot(req).await.unwrap();
    let status = response.status();
    let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body_text = String::from_utf8_lossy(&body_bytes).into_owned();
    (status, body_text)
}

#[tokio::test]
async fn p0_a_health_works() {
    let (state, _) = make_state_with_account().await;
    let app = router(state).await;
    let tid = test_tenant_id_str();
    let (status, body) = send(app, Method::GET, "/health", None, &tid).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ok");
}

// p0_a_get_account_returns_seeded_account_not_404 and
// p0_a_get_account_for_unknown_id_returns_404 removed: ADR-11 deleted the
// `GET /v1/accounts/:id` endpoint and `AccountStore` entirely — these
// tested deliberately removed functionality (see module docs).

#[tokio::test]
async fn p0_a_evaluate_order_returns_verdict() {
    // Before P0-A: this returned 500 "account not found" because
    // state.read().clone() discarded the seeded account.
    let (state, account) = make_state_with_account().await;
    let app = router(state).await;
    let req_body = serde_json::json!({
        "account_id": account.id.to_string(),
        "account_state": account,
        "symbol": "EURUSD",
        "side": "buy",
        "quantity": "1",
        "order_type": "market",
        "stop_loss": "1.05",
        "take_profit": "1.10"
    })
    .to_string();
    let tid = test_tenant_id_str();
    let (status, body) = send(
        app,
        Method::POST,
        "/v1/evaluate-order",
        Some(req_body),
        &tid,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "evaluate-order should return 200; body: {body}"
    );
    // Body should contain a decision kind (Pass/Warn/Fail/etc.) — not "account not found".
    assert!(
        !body.to_lowercase().contains("not found"),
        "body should not be a 404 error; got: {body}"
    );
}

#[tokio::test]
async fn p0_a_manual_run_returns_decision() {
    let (state, account) = make_state_with_account().await;
    let app = router(state).await;
    let req_body = serde_json::json!({
        "account_id": account.id.to_string(),
        "account_state": account
    })
    .to_string();
    let tid = test_tenant_id_str();
    let (status, body) = send(
        app,
        Method::POST,
        "/internal/v1/manual-run",
        Some(req_body),
        &tid,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "manual-run should return 200; body: {body}"
    );
    assert!(
        body.contains("decision_kind"),
        "body should contain decision_kind; got: {body}"
    );
}

#[tokio::test]
async fn p0_a_breach_report_returns_violations_array() {
    // Even with no breaches, the endpoint should return 200 + empty
    // violations array — NOT 404.
    let (state, account) = make_state_with_account().await;
    let app = router(state).await;
    let body = serde_json::json!({
        "account_id": account.id.to_string(),
        "account_state": account
    })
    .to_string();
    let tid = test_tenant_id_str();
    let (status, body) = send(
        app,
        Method::POST,
        "/internal/v1/breach-report",
        Some(body),
        &tid,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "breach-report should return 200; body: {body}"
    );
    assert!(
        body.contains("violations"),
        "body should contain violations array; got: {body}"
    );
}

#[tokio::test]
async fn p0_a_override_account_id_mismatch_returns_400() {
    let (state, account) = make_state_with_account().await;
    let app = router(state).await;
    let random_account = AccountId::new();
    let random_violation = propfirm::core::ids::ViolationId::new();
    let req_body = serde_json::json!({
        "account_id": random_account.to_string(),
        "account_state": account,
        "clears_violation_id": random_violation.to_string(),
        "reason": "broker glitch",
        "actor_id": "ops-test"
    })
    .to_string();
    let tid = test_tenant_id_str();
    let (status, _body) = send(
        app,
        Method::POST,
        "/internal/v1/override",
        Some(req_body),
        &tid,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "mismatched account_id and account_state.id must 400; got {status}"
    );
}

#[tokio::test]
async fn p0_b_internal_evaluate_input_hash_is_real_sha256() {
    // P0-B verification: the input_hash in the response must be a real
    // 64-char sha256 digest, not a 16-char SipHash.
    let (state, account) = make_state_with_account().await;
    let app = router(state).await;
    let tick_json = serde_json::json!({
        "symbol": "EURUSD",
        "quote": {
            "bid": "1.0800",
            "ask": "1.0802",
            "ts": chrono::Utc::now().to_rfc3339()
        }
    });
    let req_body = serde_json::json!({
        "account_id": account.id.to_string(),
        "account_state": account,
        "tick": tick_json
    })
    .to_string();
    let tid = test_tenant_id_str();
    let (status, body) = send(
        app,
        Method::POST,
        "/internal/v1/evaluate",
        Some(req_body),
        &tid,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "evaluate should return 200; body: {body}"
    );
    // Parse the JSON and check input_hash.
    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
    let hash = parsed["input_hash"].as_str().unwrap();
    assert!(
        hash.starts_with("sha256:"),
        "input_hash must be prefixed with sha256:; got {hash}"
    );
    // P0-B: must be 64 hex chars after the prefix (256 bits), not 16.
    assert_eq!(hash.len(), 7 + 64,
        "P0-B: input_hash must be a real 256-bit sha256 (64 hex chars after prefix); got len {} for {hash}",
        hash.len());
}

#[tokio::test]
async fn p0_a_server_state_clone_shares_underlying_store() {
    // Direct test of the P0-A fix: cloning ServerState must share the
    // underlying Arc'ed stores.
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan.clone())
        .with_tenant(TenantId::named("test"))
        .start(chrono::Utc::now())
        .unwrap();
    let state = ServerState::new(test_auth_config());
    // Clone the state — this used to discard every Arc'ed store.
    let cloned = state.clone();
    // The cloned state must still share the same idempotency backend.
    assert!(std::sync::Arc::ptr_eq(
        &state.idempotency,
        &cloned.idempotency
    ));
}
