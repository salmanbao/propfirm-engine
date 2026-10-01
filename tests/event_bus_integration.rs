//! Event-bus integration tests.
//!
//! Requires the `server` feature and a running Redis on
//! `PROPFIRM_REDIS__URL` (default `redis://127.0.0.1:6380`).
//!
//! Run with:
//!   cargo test --features server --test event_bus_integration

#![cfg(feature = "server")]

use propfirm::persistence::redis_store::{
    connect as redis_connect,
    event_bus::{EvaluateRequestPayload, EvaluateResponsePayload, EventBusResult},
    RedisEventBus,
};
use propfirm::settings::Settings;
use std::time::Duration;
use uuid::Uuid;

#[tokio::test]
async fn event_bus_round_trip() {
    let mut settings = Settings::load().expect("settings");
    settings.event_bus.request_stream = format!("propfirm:evaluate:requests:{}", Uuid::new_v4());
    settings.event_bus.consumer_group = format!("propfirm-worker:{}", Uuid::new_v4());
    let redis_conn = redis_connect(&settings.redis).await.expect("redis connect");
    let bus = RedisEventBus::new(redis_conn, settings.event_bus.clone());
    bus.ensure_group().await.expect("ensure group");

    let tenant_id = "test-tenant";
    let account_id = Uuid::new_v4().to_string();
    let request_id = Uuid::new_v4().to_string();
    let payload = serde_json::json!({
        "account_id": account_id,
        "account_state": {
            "id": account_id,
            "tenant_id": tenant_id,
            "plan": {"name":"FTMO Phase 1"},
            "status": "Active",
            "balance": "10000",
            "equity": "10000",
        },
        "tick": {
            "symbol": "EURUSD",
            "quote": {"bid": "1.08", "ask": "1.0802", "ts": chrono::Utc::now().to_rfc3339()}
        },
        "open_positions": [],
        "today_trades": []
    });

    let request = EvaluateRequestPayload {
        request_id: request_id.clone(),
        tenant_id: tenant_id.to_string(),
        account_id,
        payload,
        submitted_at: chrono::Utc::now().to_rfc3339(),
    };

    bus.produce_request(&request)
        .await
        .expect("produce request");

    let mut found = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !found && std::time::Instant::now() < deadline {
        match bus.consume_request("test-consumer").await {
            EventBusResult::Consumed { stream_id, payload } => {
                assert_eq!(payload.request_id, request_id);
                let response = EvaluateResponsePayload {
                    request_id: request_id.clone(),
                    decision_kind: "Pass".into(),
                    input_hash: "abc123".into(),
                    account_state: serde_json::json!({"status": "Active"}),
                    violations: vec![],
                    processed_at: chrono::Utc::now().to_rfc3339(),
                    error: None,
                };
                bus.produce_response(&response)
                    .await
                    .expect("produce response");
                bus.ack(&stream_id).await.expect("ack");
                found = true;
            }
            EventBusResult::Empty => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            EventBusResult::Error(e) => panic!("consume error: {e}"),
            EventBusResult::Produced(_) => {}
        }
    }

    assert!(found, "did not consume request within deadline");
}

#[tokio::test]
async fn event_bus_multiple_requests_ordered() {
    let mut settings = Settings::load().expect("settings");
    settings.event_bus.request_stream = format!("propfirm:evaluate:requests:{}", Uuid::new_v4());
    settings.event_bus.consumer_group = format!("propfirm-worker:{}", Uuid::new_v4());
    let redis_conn = redis_connect(&settings.redis).await.expect("redis connect");
    let bus = RedisEventBus::new(redis_conn, settings.event_bus.clone());
    bus.ensure_group().await.expect("ensure group");

    let tenant_id = "test-tenant";
    let mut request_ids = Vec::new();

    for _i in 0..3u8 {
        let account_id = Uuid::new_v4().to_string();
        let request_id = Uuid::new_v4().to_string();
        request_ids.push(request_id.clone());

        let payload = serde_json::json!({
            "account_id": account_id,
            "account_state": {
                "id": account_id,
                "tenant_id": tenant_id,
                "plan": {"name":"FTMO Phase 1"},
                "status": "Active",
                "balance": "10000",
                "equity": "10000",
            },
            "tick": {
                "symbol": "EURUSD",
                "quote": {"bid": "1.08", "ask": "1.0802", "ts": chrono::Utc::now().to_rfc3339()}
            },
            "open_positions": [],
            "today_trades": []
        });

        let request = EvaluateRequestPayload {
            request_id: request_id.clone(),
            tenant_id: tenant_id.to_string(),
            account_id,
            payload,
            submitted_at: chrono::Utc::now().to_rfc3339(),
        };

        bus.produce_request(&request)
            .await
            .expect("produce request");
    }

    let mut seen = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while seen.len() < request_ids.len() && std::time::Instant::now() < deadline {
        match bus.consume_request("test-consumer").await {
            EventBusResult::Consumed { stream_id, payload } => {
                seen.push(payload.request_id.clone());
                let response = EvaluateResponsePayload {
                    request_id: payload.request_id.clone(),
                    decision_kind: "Pass".into(),
                    input_hash: "abc".into(),
                    account_state: serde_json::json!({"status": "Active"}),
                    violations: vec![],
                    processed_at: chrono::Utc::now().to_rfc3339(),
                    error: None,
                };
                bus.produce_response(&response)
                    .await
                    .expect("produce response");
                bus.ack(&stream_id).await.expect("ack");
            }
            EventBusResult::Empty => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            EventBusResult::Error(e) => panic!("consume error: {e}"),
            EventBusResult::Produced(_) => {}
        }
    }

    assert_eq!(seen.len(), request_ids.len());
    for expected in &request_ids {
        assert!(seen.contains(expected), "missing request_id {expected}");
    }
}

#[tokio::test]
async fn event_bus_normal_tick_evaluation() {
    let mut settings = Settings::load().expect("settings");
    settings.event_bus.request_stream = format!("propfirm:evaluate:requests:{}", Uuid::new_v4());
    settings.event_bus.consumer_group = format!("propfirm-worker:{}", Uuid::new_v4());
    let redis_conn = redis_connect(&settings.redis).await.expect("redis connect");
    let bus = RedisEventBus::new(redis_conn, settings.event_bus.clone());
    bus.ensure_group().await.expect("ensure group");

    let tenant_id = "test-tenant";
    let account_id = Uuid::new_v4().to_string();
    let request_id = Uuid::new_v4().to_string();

    let payload = serde_json::json!({
        "account_id": account_id,
        "account_state": {
            "id": account_id,
            "tenant_id": tenant_id,
            "plan": {"name":"FTMO Phase 1"},
            "status": "Active",
            "balance": "10000",
            "equity": "10000",
        },
        "tick": {
            "symbol": "EURUSD",
            "quote": {"bid": "1.08", "ask": "1.0802", "ts": chrono::Utc::now().to_rfc3339()}
        },
        "open_positions": [],
        "today_trades": []
    });

    let request = EvaluateRequestPayload {
        request_id: request_id.clone(),
        tenant_id: tenant_id.to_string(),
        account_id,
        payload,
        submitted_at: chrono::Utc::now().to_rfc3339(),
    };

    bus.produce_request(&request)
        .await
        .expect("produce request");

    let mut response_payload = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while response_payload.is_none() && std::time::Instant::now() < deadline {
        match bus.consume_request("test-consumer").await {
            EventBusResult::Consumed { stream_id, payload } => {
                assert_eq!(payload.request_id, request_id);
                let response = EvaluateResponsePayload {
                    request_id: request_id.clone(),
                    decision_kind: "Pass".into(),
                    input_hash: "abc123".into(),
                    account_state: serde_json::json!({"status": "Active"}),
                    violations: vec![],
                    processed_at: chrono::Utc::now().to_rfc3339(),
                    error: None,
                };
                bus.produce_response(&response)
                    .await
                    .expect("produce response");
                bus.ack(&stream_id).await.expect("ack");
                response_payload = Some(response);
            }
            EventBusResult::Empty => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            EventBusResult::Error(e) => panic!("consume error: {e}"),
            EventBusResult::Produced(_) => {}
        }
    }

    let response = response_payload.expect("no response within deadline");
    assert_eq!(response.request_id, request_id);
    assert_eq!(response.decision_kind, "Pass");
    assert!(response.error.is_none());
}

#[tokio::test]
async fn event_bus_drawdown_breach_evaluation() {
    let mut settings = Settings::load().expect("settings");
    settings.event_bus.request_stream = format!("propfirm:evaluate:requests:{}", Uuid::new_v4());
    settings.event_bus.consumer_group = format!("propfirm-worker:{}", Uuid::new_v4());
    let redis_conn = redis_connect(&settings.redis).await.expect("redis connect");
    let bus = RedisEventBus::new(redis_conn, settings.event_bus.clone());
    bus.ensure_group().await.expect("ensure group");

    let tenant_id = "test-tenant";
    let account_id = Uuid::new_v4().to_string();
    let request_id = Uuid::new_v4().to_string();

    // Equity dropped to 9000 from 10000 = 10% drawdown, FTMO Phase 1 limit is 5%.
    let payload = serde_json::json!({
        "account_id": account_id,
        "account_state": {
            "id": account_id,
            "tenant_id": tenant_id,
            "plan": {"name":"FTMO Phase 1"},
            "status": "Active",
            "balance": "9000",
            "equity": "9000",
        },
        "tick": {
            "symbol": "EURUSD",
            "quote": {"bid": "1.08", "ask": "1.0802", "ts": chrono::Utc::now().to_rfc3339()}
        },
        "open_positions": [],
        "today_trades": []
    });

    let request = EvaluateRequestPayload {
        request_id: request_id.clone(),
        tenant_id: tenant_id.to_string(),
        account_id,
        payload,
        submitted_at: chrono::Utc::now().to_rfc3339(),
    };

    bus.produce_request(&request)
        .await
        .expect("produce request");

    let mut response_payload = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while response_payload.is_none() && std::time::Instant::now() < deadline {
        match bus.consume_request("test-consumer").await {
            EventBusResult::Consumed { stream_id, payload } => {
                assert_eq!(payload.request_id, request_id);
                let response = EvaluateResponsePayload {
                    request_id: request_id.clone(),
                    decision_kind: "Fail".into(),
                    input_hash: "def456".into(),
                    account_state: serde_json::json!({"status": "Failed"}),
                    violations: vec![serde_json::json!({"kind": "MaxDrawdown"})],
                    processed_at: chrono::Utc::now().to_rfc3339(),
                    error: None,
                };
                bus.produce_response(&response)
                    .await
                    .expect("produce response");
                bus.ack(&stream_id).await.expect("ack");
                response_payload = Some(response);
            }
            EventBusResult::Empty => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            EventBusResult::Error(e) => panic!("consume error: {e}"),
            EventBusResult::Produced(_) => {}
        }
    }

    let response = response_payload.expect("no response within deadline");
    assert_eq!(response.request_id, request_id);
    assert_eq!(response.decision_kind, "Fail");
    assert!(!response.violations.is_empty());
    assert!(response.error.is_none());
}

#[tokio::test]
async fn event_bus_override_recovery_evaluation() {
    let mut settings = Settings::load().expect("settings");
    settings.event_bus.request_stream = format!("propfirm:evaluate:requests:{}", Uuid::new_v4());
    settings.event_bus.consumer_group = format!("propfirm-worker:{}", Uuid::new_v4());
    let redis_conn = redis_connect(&settings.redis).await.expect("redis connect");
    let bus = RedisEventBus::new(redis_conn, settings.event_bus.clone());
    bus.ensure_group().await.expect("ensure group");

    let tenant_id = "test-tenant";
    let account_id = Uuid::new_v4().to_string();
    let request_id = Uuid::new_v4().to_string();

    let payload = serde_json::json!({
        "account_id": account_id,
        "account_state": {
            "id": account_id,
            "tenant_id": tenant_id,
            "plan": {"name":"FTMO Phase 1"},
            "status": "Failed",
            "balance": "9000",
            "equity": "9000",
        },
        "tick": {
            "symbol": "EURUSD",
            "quote": {"bid": "1.08", "ask": "1.0802", "ts": chrono::Utc::now().to_rfc3339()}
        },
        "open_positions": [],
        "today_trades": []
    });

    let request = EvaluateRequestPayload {
        request_id: request_id.clone(),
        tenant_id: tenant_id.to_string(),
        account_id,
        payload,
        submitted_at: chrono::Utc::now().to_rfc3339(),
    };

    bus.produce_request(&request)
        .await
        .expect("produce request");

    let mut response_payload = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while response_payload.is_none() && std::time::Instant::now() < deadline {
        match bus.consume_request("test-consumer").await {
            EventBusResult::Consumed { stream_id, payload } => {
                assert_eq!(payload.request_id, request_id);
                let response = EvaluateResponsePayload {
                    request_id: request_id.clone(),
                    decision_kind: "Active".into(),
                    input_hash: "override123".into(),
                    account_state: serde_json::json!({"status": "Active"}),
                    violations: vec![],
                    processed_at: chrono::Utc::now().to_rfc3339(),
                    error: None,
                };
                bus.produce_response(&response)
                    .await
                    .expect("produce response");
                bus.ack(&stream_id).await.expect("ack");
                response_payload = Some(response);
            }
            EventBusResult::Empty => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            EventBusResult::Error(e) => panic!("consume error: {e}"),
            EventBusResult::Produced(_) => {}
        }
    }

    let response = response_payload.expect("no response within deadline");
    assert_eq!(response.request_id, request_id);
    assert_eq!(response.decision_kind, "Active");
    assert!(response.error.is_none());
}
