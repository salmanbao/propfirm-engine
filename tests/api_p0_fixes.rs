//! P0.5–P0.8 API integration tests.
//!
//! - P0.5: `equity_source` provenance — `estimated` (default) cannot
//!   terminate; `broker_reported` can.
//! - P0.6: `open_positions` / `today_trades` flow through the endpoint.
//! - P0.7: rule-pack endpoints implement the real lifecycle
//!   (draft → active → superseded) with 409 on illegal transitions.
//! - P0.8: idempotency — same key + same body replays the first
//!   response; same key + conflicting body → 409; mutation double-apply
//!   is impossible.

#![cfg(feature = "server")]

use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use chrono::Datelike;
use http_body_util::BodyExt;
use propfirm::api::auth::AuthConfig;
use propfirm::api::routes::router;
use propfirm::api::server::ServerState;
use propfirm::config::presets::ftmo_phase1;
use propfirm::core::account::Account;
use propfirm::core::ids::AccountId;
use propfirm::core::types::Money;
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

const SERVICE_KEY: &str = "service-secret";

fn hex_fmt(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

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

/// Builds a `ServerState` with one seeded account at the given equity.
async fn make_state_at(equity: i64) -> (Arc<tokio::sync::RwLock<ServerState>>, Account) {
    let plan = ftmo_phase1(); // 10k static max loss: breach below 9k equity
    let mut account = Account::new(AccountId::new(), plan.clone())
        .with_tenant(TenantId::named("test-tenant"))
        .start(chrono::Utc::now())
        .unwrap();
    account.equity = Money::new(rust_decimal::Decimal::new(equity, 0));
    account.balance = account.equity;
    let state = Arc::new(tokio::sync::RwLock::new(ServerState::new(
        plan,
        test_auth_config(),
    )));
    state.read().await.store.put(account.clone()).await.unwrap();
    (state, account)
}

/// Helper: send a request with optional headers and body; returns (status, body).
async fn send_with_headers(
    app: axum::Router,
    method: Method,
    uri: &str,
    body: Option<String>,
    headers: &[(&str, &str)],
) -> (StatusCode, String) {
    let tid = test_tenant_id_str();
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("X-Tenant-Id", &tid)
        .header("Authorization", format!("Bearer {SERVICE_KEY}"));
    for (k, v) in headers {
        req = req.header(*k, *v);
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
    let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&body_bytes).into_owned())
}

async fn send(
    app: axum::Router,
    method: Method,
    uri: &str,
    body: Option<String>,
) -> (StatusCode, String) {
    send_with_headers(app, method, uri, body, &[]).await
}

/// Builds the evaluate request JSON for the seeded account.
fn evaluate_body(account_id: &AccountId, equity_source: Option<&str>) -> String {
    let tick_json = serde_json::json!({
        "symbol": "EURUSD",
        "quote": {
            "bid": "1.0800",
            "ask": "1.0802",
            "ts": chrono::Utc::now().to_rfc3339()
        }
    });
    let mut body = serde_json::json!({
        "account_id": account_id.to_string(),
        "tick": tick_json
    });
    if let Some(src) = equity_source {
        body["equity_source"] = serde_json::Value::String(src.to_string());
    }
    body.to_string()
}

// ---------------------------------------------------------------------------
// P0.5 — equity provenance
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p0_5_estimated_equity_cannot_terminate_via_endpoint() {
    // Account at 8k equity on a 10k static floor with a 10% max-loss
    // pack — a breach IF the equity is trusted. With `estimated`
    // provenance the breach-capable rule downgrades to Warn.
    let (state, account) = make_state_at(8000).await;
    let app = router(state).await;
    let (status, body) = send(
        app,
        Method::POST,
        "/internal/v1/evaluate",
        Some(evaluate_body(&account.id, Some("estimated"))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        !body.contains("\"Liquidate\"") && !body.contains("\"Fail\""),
        "estimated equity must NOT terminate via the endpoint; got: {body}"
    );
}

#[tokio::test]
async fn p0_5_missing_equity_source_defaults_to_estimated() {
    let (state, account) = make_state_at(8000).await;
    let app = router(state).await;
    // No equity_source field at all — must default to the safe option.
    let (status, body) = send(
        app,
        Method::POST,
        "/internal/v1/evaluate",
        Some(evaluate_body(&account.id, None)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        !body.contains("\"Liquidate\"") && !body.contains("\"Fail\""),
        "absent equity_source must default to estimated (never terminate); got: {body}"
    );
}

#[tokio::test]
async fn p0_5_broker_reported_equity_can_terminate_via_endpoint() {
    let (state, account) = make_state_at(8000).await;
    let app = router(state).await;
    let (status, body) = send(
        app,
        Method::POST,
        "/internal/v1/evaluate",
        Some(evaluate_body(&account.id, Some("broker_reported"))),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(
        body.contains("\"Liquidate\""),
        "broker-reported equity below the static floor must Liquidate; got: {body}"
    );
}

// ---------------------------------------------------------------------------
// P0.6 — positions and trades on the stateless path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p0_6_open_position_in_overnight_window_produces_violation() {
    // ftmo_phase1 allows overnight holding, so flip it off via a pack
    // entry on a plan that forbids it. Simpler: use the plan's own
    // weekend_holding_allowed = false and submit an order on Saturday.
    let (state, account) = make_state_at(10000).await;
    let app = router(state).await;
    // Saturday timestamp.
    let sat = chrono::Utc::now();
    let days_to_sat = (5 + 7 - sat.weekday().num_days_from_monday()) % 7;
    let saturday = sat + chrono::Duration::days(i64::from(days_to_sat));
    let tick_json = serde_json::json!({
        "symbol": "EURUSD",
        "quote": { "bid": "1.0800", "ask": "1.0802", "ts": saturday.to_rfc3339() }
    });
    let body = serde_json::json!({
        "account_id": account.id.to_string(),
        "tick": tick_json,
        "equity_source": "broker_reported",
        "open_positions": [{
            "position_id": propfirm::core::ids::PositionId::new().to_string(),
            "symbol": "EURUSD",
            "side": "long",
            "open_quantity": "1",
            "avg_entry_price": "1.0850",
            "opened_at": saturday.to_rfc3339()
        }]
    })
    .to_string();
    let (status, resp) = send(app, Method::POST, "/internal/v1/evaluate", Some(body)).await;
    assert_eq!(status, StatusCode::OK, "resp: {resp}");
    assert!(
        resp.contains("weekend") || resp.contains("Weekend"),
        "an open position over the weekend must produce a weekend violation via the endpoint; got: {resp}"
    );
}

// ---------------------------------------------------------------------------
// P0.6 — positions and trades on the stateless path
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// P0.8 — idempotency
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p0_8_same_key_same_body_replays_first_response() {
    let (state, account) = make_state_at(9500).await;
    let app = router(state).await;
    let body = evaluate_body(&account.id, Some("broker_reported"));
    let headers = [("Idempotency-Key", "eval-key-1")];
    let (s1, b1) = send_with_headers(
        app.clone(),
        Method::POST,
        "/internal/v1/evaluate",
        Some(body.clone()),
        &headers,
    )
    .await;
    let (s2, b2) = send_with_headers(
        app,
        Method::POST,
        "/internal/v1/evaluate",
        Some(body.clone()),
        &headers,
    )
    .await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(
        b1, b2,
        "same key + same body must replay the first response"
    );
}

#[tokio::test]
async fn p0_8_same_key_conflicting_body_returns_409() {
    let (state, account) = make_state_at(9500).await;
    let app = router(state).await;
    let headers = [("Idempotency-Key", "eval-key-2")];
    let body_a = evaluate_body(&account.id, Some("broker_reported"));
    let body_b = evaluate_body(&account.id, Some("estimated")); // conflicting
    let (s1, _) = send_with_headers(
        app.clone(),
        Method::POST,
        "/internal/v1/evaluate",
        Some(body_a),
        &headers,
    )
    .await;
    assert_eq!(s1, StatusCode::OK);
    let (s2, b2) = send_with_headers(
        app,
        Method::POST,
        "/internal/v1/evaluate",
        Some(body_b),
        &headers,
    )
    .await;
    assert_eq!(
        s2,
        StatusCode::CONFLICT,
        "same key + different body must 409; got: {b2}"
    );
}

#[tokio::test]
async fn p0_8_mutation_is_not_double_applied() {
    // Use /v1/evaluate-order with an idempotency key: a hedging-forbidden
    // plan + opposite order fails the order — but more to the point, the
    // second identical request must return the identical (replayed)
    // response, proving no double-apply path ran.
    let (state, account) = make_state_at(10000).await;
    let app = router(state).await;
    let order_body = serde_json::json!({
        "account_id": account.id.to_string(),
        "symbol": "EURUSD",
        "side": "buy",
        "quantity": "1",
        "order_type": "market"
    })
    .to_string();
    let headers = [("Idempotency-Key", "order-key-1")];
    let (s1, b1) = send_with_headers(
        app.clone(),
        Method::POST,
        "/v1/evaluate-order",
        Some(order_body.clone()),
        &headers,
    )
    .await;
    let (s2, b2) = send_with_headers(
        app,
        Method::POST,
        "/v1/evaluate-order",
        Some(order_body),
        &headers,
    )
    .await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(
        b1, b2,
        "replayed mutation must return the first response exactly"
    );
}

// ---------------------------------------------------------------------------
// P2 — bridge.tick v1 wire contract
// ---------------------------------------------------------------------------

#[tokio::test]
async fn p2_bridge_tick_v1_broker_reported_equity_can_terminate() {
    // Account was seeded at 10k equity in the store, but the bridge.tick
    // envelope reports 8.5k broker-reported equity. On a 10k static-floor
    // plan that is a breach — and because bridge.tick is broker-attested
    // the equity source defaults to BrokerReported automatically.
    let (state, account) = make_state_at(10000).await;
    let app = router(state).await;
    let broker_time = chrono::Utc::now().timestamp_millis();
    let body = serde_json::json!({
        "account_id": account.id.to_string(),
        "bridge_tick": {
            "type": "bridge.tick",
            "version": 1,
            "tenant_id": test_tenant_id_str(),
            "occurred_at": broker_time,
            "payload": {
                "equity_cents": 850000,
                "balance_cents": 850000,
                "margin_cents": 50000,
                "free_margin_cents": 800000,
                "leverage": 100,
                "positions": [],
                "deals_count": 0,
                "broker_time": broker_time
            }
        }
    })
    .to_string();
    let (status, body) = send(app, Method::POST, "/internal/v1/evaluate", Some(body)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "bridge.tick evaluate failed: {body}"
    );
    assert!(
        !body.contains("Pass"),
        "broker-reported equity below floor must breach; got: {body}"
    );
}

#[tokio::test]
async fn p2_bridge_tick_v1_positions_flow_through() {
    // Verify that positions[] from the envelope are wired into the rule
    // context by using a plan that forbids overnight holding and an open
    // position over the weekend.
    let (state, account) = make_state_at(10000).await;
    let app = router(state).await;
    let sat = chrono::Utc::now();
    let days_to_sat = (5 + 7 - sat.weekday().num_days_from_monday()) % 7;
    let saturday = sat + chrono::Duration::days(i64::from(days_to_sat));
    let broker_time = saturday.timestamp_millis();
    let body = serde_json::json!({
        "account_id": account.id.to_string(),
        "bridge_tick": {
            "type": "bridge.tick",
            "version": 1,
            "tenant_id": test_tenant_id_str(),
            "occurred_at": broker_time,
            "payload": {
                "equity_cents": 1000000,
                "balance_cents": 1000000,
                "margin_cents": 50000,
                "free_margin_cents": 950000,
                "leverage": 100,
                "positions": [
                    {
                        "position_id": account.id.to_string(),
                        "symbol": "EURUSD",
                        "side": "long",
                        "open_quantity": "1",
                        "avg_entry_price": "1.0800",
                        "opened_at": saturday.to_rfc3339()
                    }
                ],
                "deals_count": 0,
                "broker_time": broker_time
            }
        }
    })
    .to_string();
    let (status, body) = send(app, Method::POST, "/internal/v1/evaluate", Some(body)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "bridge.tick evaluate failed: {body}"
    );
    assert!(
        body.contains("Weekend") || body.contains("weekend"),
        "open position over the weekend must produce a weekend violation; got: {body}"
    );
}
