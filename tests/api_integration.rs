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
//! - `GET /v1/accounts/:id` returns the account we seeded (not 404)
//! - `POST /v1/evaluate-order` returns a verdict (not "account not found")
//! - `POST /internal/v1/evaluate` returns a stateless verdict with input_hash
//! - `POST /internal/v1/manual-run` returns a decision
//! - `GET /internal/v1/breach-report/:account_id` returns the breach log

#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use http_body_util::BodyExt; // for collect()
use propfirm::api::routes::router;
use propfirm::api::server::ServerState;
use propfirm::config::presets::ftmo_phase1;
use propfirm::core::account::Account;
use propfirm::core::ids::AccountId;
use propfirm::core::types::dec;
use propfirm::persistence::traits::AccountStore;
use propfirm::tenant::TenantId;
use std::sync::Arc;
use tower::ServiceExt;

fn test_tenant_id() -> TenantId {
    TenantId::named("test-tenant")
}

fn test_tenant_id_str() -> String {
    test_tenant_id().to_string()
}

/// Builds a `ServerState` with one seeded account.
async fn make_state_with_account() -> (Arc<parking_lot::RwLock<ServerState>>, Account) {
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan.clone())
        .with_tenant(test_tenant_id())
        .start(chrono::Utc::now())
        .unwrap();
    let state = Arc::new(parking_lot::RwLock::new(ServerState::new(plan)));
    state.read().store.put(account.clone()).unwrap();
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
        .header("X-Tenant-Id", tenant_id);
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
    let app = router(state);
    let tid = test_tenant_id_str();
    let (status, body) = send(app, Method::GET, "/health", None, &tid).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ok");
}

#[tokio::test]
async fn p0_a_get_account_returns_seeded_account_not_404() {
    // Before P0-A: this returned 404 because state.read().clone()
    // created a brand-new empty InMemoryStore.
    let (state, account) = make_state_with_account().await;
    let app = router(state);
    let uri = format!("/v1/accounts/{}", account.id);
    let tid = test_tenant_id_str();
    let (status, body) = send(app, Method::GET, &uri, None, &tid).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "GET account should return 200; body: {body}"
    );
    // Body should contain the account's balance (10_000).
    assert!(
        body.contains("10000"),
        "body should contain account balance; got: {body}"
    );
}

#[tokio::test]
async fn p0_a_get_account_for_unknown_id_returns_404() {
    let (state, _) = make_state_with_account().await;
    let app = router(state);
    let random_id = AccountId::new();
    let uri = format!("/v1/accounts/{random_id}");
    let tid = test_tenant_id_str();
    let (status, _body) = send(app, Method::GET, &uri, None, &tid).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn p0_a_evaluate_order_returns_verdict() {
    // Before P0-A: this returned 500 "account not found" because
    // state.read().clone() discarded the seeded account.
    let (state, account) = make_state_with_account().await;
    let app = router(state);
    let req_body = serde_json::json!({
        "account_id": account.id.to_string(),
        "symbol": "EURUSD",
        "side": "buy",
        "quantity": "1",
        "order_type": "market",
        "stop_loss": "1.05",
        "take_profit": "1.10"
    })
    .to_string();
    let tid = test_tenant_id_str();
    let (status, body) = send(app, Method::POST, "/v1/evaluate-order", Some(req_body), &tid).await;
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
    let app = router(state);
    let req_body = serde_json::json!({
        "account_id": account.id.to_string()
    })
    .to_string();
    let tid = test_tenant_id_str();
    let (status, body) = send(app, Method::POST, "/internal/v1/manual-run", Some(req_body), &tid).await;
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
    let app = router(state);
    let uri = format!("/internal/v1/breach-report/{}", account.id);
    let tid = test_tenant_id_str();
    let (status, body) = send(app, Method::GET, &uri, None, &tid).await;
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
async fn p0_a_override_for_unknown_account_returns_404() {
    let (state, _) = make_state_with_account().await;
    let app = router(state);
    let random_account = AccountId::new();
    let random_violation = propfirm::core::ids::ViolationId::new();
    let req_body = serde_json::json!({
        "account_id": random_account.to_string(),
        "clears_violation_id": random_violation.to_string(),
        "reason": "broker glitch",
        "actor_id": "ops-test"
    })
    .to_string();
    let tid = test_tenant_id_str();
    let (status, _body) = send(app, Method::POST, "/internal/v1/override", Some(req_body), &tid).await;
    // Override for unknown account → 404 or 500 (the pipeline returns
    // NotFound). Either way, NOT 200 with an empty body.
    assert!(
        status == StatusCode::NOT_FOUND || status == StatusCode::INTERNAL_SERVER_ERROR,
        "override for unknown account should fail; got {status}"
    );
}

#[tokio::test]
async fn p0_b_internal_evaluate_input_hash_is_real_sha256() {
    // P0-B verification: the input_hash in the response must be a real
    // 64-char sha256 digest, not a 16-char SipHash.
    let (state, account) = make_state_with_account().await;
    let app = router(state);
    let tick_json = serde_json::json!({
        "symbol": "EURUSD",
        "quote": {
            "bid": "1.0800",
            "ask": "1.0802",
            "ts": chrono::Utc::now().to_rfc3339()
        }
    });
    let rule_pack_json = serde_json::json!({
        "id": "test-pack-v1",
        "version": 1,
        "tenant_id": "00000000-0000-0000-0000-000000000001",
        "lifecycle": "active",
        "effective_from": chrono::Utc::now().to_rfc3339(),
        "superseded_by": serde_json::Value::Null,
        "description": "test pack",
        "rules": [{
            "id": "max_total_loss",
            "kind": "max_drawdown",
            "basis": "static",
            "unit": "percent",
            "value": "0.10",
            "tolerance_cents": 1,
            "early_warning_pct": "0.80",
            "priority": 1000,
            "enabled": true,
            "params_json": "{}"
        }],
        "initial_balance": dec!(10_000),
        "leverage": 100,
        "profit_target_pct": "0.10"
    });
    let req_body = serde_json::json!({
        "account_id": account.id.to_string(),
        "rule_pack": rule_pack_json,
        "tick": tick_json
    })
    .to_string();
    let tid = test_tenant_id_str();
    let (status, body) = send(app, Method::POST, "/internal/v1/evaluate", Some(req_body), &tid).await;
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
    // underlying InMemoryStore (not re-instantiate an empty one).
    use propfirm::persistence::traits::AccountStore;
    let plan = ftmo_phase1();
    let account = Account::new(AccountId::new(), plan.clone())
        .with_tenant(TenantId::named("test"))
        .start(chrono::Utc::now())
        .unwrap();
    let state = ServerState::new(plan);
    state.store.put(account.clone()).unwrap();
    // Clone the state — this used to discard the seeded account.
    let cloned = state.clone();
    // The cloned state should still see the account.
    let retrieved = cloned.store.get(account.id).unwrap();
    assert!(
        retrieved.is_some(),
        "P0-A: ServerState::clone must share the underlying store; got None"
    );
    assert_eq!(retrieved.unwrap().id, account.id);
}
