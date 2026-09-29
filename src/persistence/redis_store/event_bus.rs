//! Redis Streams event bus.
//!
//! The engine is designed for **async communication** with the platform
//! backend: rather than each evaluation being a synchronous HTTP round
//! trip, the platform writes a request to the
//! `propfirm:evaluate:requests` stream, and a fleet of
//! `propfirm-worker` processes consume the stream via consumer groups,
//! evaluate each request, and XADD the result to the
//! `propfirm:evaluate:responses` stream. The platform backend then
//! reads the response (correlated by `request_id` field).
//!
//! ## Why streams instead of pub/sub
//!
//! Pub/sub drops messages when there are no subscribers; streams persist
//! messages until trimmed, so a worker restart never loses a request.
//! Consumer groups add at-least-once delivery, idle-detection for
//! claim/retry, and per-consumer pending entries lists (PEL).
//!
//! ## Wire format on the stream
//!
//! Each XADD entry has these fields:
//! - `request_id` — UUID (correlation key for the response)
//! - `payload` — JSON-serialized request/response body

use std::time::Duration;
use uuid::Uuid;

use crate::persistence::redis_store::RedisConn;
use crate::settings::EventBusSettings;

type StreamEntry = (String, Vec<(String, String)>);
type StreamGroup = (String, Vec<StreamEntry>);
type XReadRaw = redis::RedisResult<Option<Vec<StreamGroup>>>;

/// Wire shape for an evaluation request on the bus.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EvaluateRequestPayload {
    pub request_id: String,
    pub tenant_id: String,
    pub account_id: String,
    pub payload: serde_json::Value,
    pub submitted_at: String,
}

/// Wire shape for an evaluation response on the bus.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EvaluateResponsePayload {
    pub request_id: String,
    pub decision_kind: String,
    pub input_hash: String,
    pub account_state: serde_json::Value,
    pub violations: Vec<serde_json::Value>,
    pub processed_at: String,
    pub error: Option<String>,
}

/// Result of an event-bus operation.
#[derive(Debug)]
pub enum EventBusResult {
    /// Successfully XADD-ed a message; returns the stream ID.
    Produced(String),
    /// Successfully XREADGROUP-d a message.
    Consumed {
        /// Redis stream message id, e.g. `1234-0`.
        stream_id: String,
        /// The deserialized request payload.
        payload: EvaluateRequestPayload,
    },
    /// No message available within the block window.
    Empty,
    /// Redis returned an error.
    Error(String),
}

/// Redis Streams event bus — both consumer (worker) and producer (worker).
#[derive(Clone)]
pub struct RedisEventBus {
    conn: RedisConn,
    settings: EventBusSettings,
}

impl RedisEventBus {
    #[must_use]
    pub fn new(conn: RedisConn, settings: EventBusSettings) -> Self {
        RedisEventBus { conn, settings }
    }

    /// Clone the underlying connection (cheap for MultiplexedConnection).
    #[must_use]
    fn conn(&self) -> RedisConn {
        self.conn.clone()
    }

    /// Ensure the consumer group exists. Creates it if missing.
    /// Idempotent — ignores `BUSYGROUP` errors.
    pub async fn ensure_group(&self) -> Result<(), redis::RedisError> {
        let stream = self.settings.request_stream.clone();
        let group = self.settings.consumer_group.clone();
        let start_id = "$";
        let conn = self.conn();
        match conn {
            RedisConn::Single(mut c) => {
                let _: () = redis::cmd("XGROUP")
                    .arg("CREATE")
                    .arg(&stream)
                    .arg(&group)
                    .arg(start_id)
                    .arg("MKSTREAM")
                    .query_async(&mut c)
                    .await
                    .unwrap_or(());
            }
            RedisConn::Cluster(arc) => {
                let mut c = arc.lock().await;
                let _: () = redis::cmd("XGROUP")
                    .arg("CREATE")
                    .arg(&stream)
                    .arg(&group)
                    .arg(start_id)
                    .arg("MKSTREAM")
                    .query_async(&mut *c)
                    .await
                    .unwrap_or(());
            }
        }
        Ok(())
    }

    /// Produce an evaluation request to the request stream.
    pub async fn produce_request(
        &self,
        payload: &EvaluateRequestPayload,
    ) -> Result<String, redis::RedisError> {
        let stream = self.settings.request_stream.clone();
        let serialized = serde_json::to_string(payload).unwrap_or_default();
        let conn = self.conn();
        match conn {
            RedisConn::Single(mut c) => {
                let id: String = redis::cmd("XADD")
                    .arg(&stream)
                    .arg("*")
                    .arg("request_id")
                    .arg(&payload.request_id)
                    .arg("payload")
                    .arg(&serialized)
                    .query_async(&mut c)
                    .await?;
                Ok(id)
            }
            RedisConn::Cluster(arc) => {
                let mut c = arc.lock().await;
                let id: String = redis::cmd("XADD")
                    .arg(&stream)
                    .arg("*")
                    .arg("request_id")
                    .arg(&payload.request_id)
                    .arg("payload")
                    .arg(&serialized)
                    .query_async(&mut *c)
                    .await?;
                Ok(id)
            }
        }
    }

    /// Produce an evaluation response to the response stream.
    pub async fn produce_response(
        &self,
        payload: &EvaluateResponsePayload,
    ) -> Result<String, redis::RedisError> {
        let stream = self.settings.response_stream.clone();
        let serialized = serde_json::to_string(payload).unwrap_or_default();
        let conn = self.conn();
        match conn {
            RedisConn::Single(mut c) => {
                let id: String = redis::cmd("XADD")
                    .arg(&stream)
                    .arg("*")
                    .arg("request_id")
                    .arg(&payload.request_id)
                    .arg("payload")
                    .arg(&serialized)
                    .query_async(&mut c)
                    .await?;
                Ok(id)
            }
            RedisConn::Cluster(arc) => {
                let mut c = arc.lock().await;
                let id: String = redis::cmd("XADD")
                    .arg(&stream)
                    .arg("*")
                    .arg("request_id")
                    .arg(&payload.request_id)
                    .arg("payload")
                    .arg(&serialized)
                    .query_async(&mut *c)
                    .await?;
                Ok(id)
            }
        }
    }

    /// Consume one batch of messages from the request stream via the
    /// consumer group. Blocks for `block_ms` if no messages are available.
    pub async fn consume_request(&self) -> EventBusResult {
        let consumer = if self.settings.consumer_name.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            self.settings.consumer_name.clone()
        };
        let block_ms = self.settings.block_ms;
        let group = self.settings.consumer_group.clone();
        let stream = self.settings.request_stream.clone();

        let conn = self.conn();
        let raw: XReadRaw = match conn {
            RedisConn::Single(mut c) => {
                redis::cmd("XREADGROUP")
                    .arg("GROUP")
                    .arg(&group)
                    .arg(&consumer)
                    .arg("COUNT")
                    .arg(1i64)
                    .arg("BLOCK")
                    .arg(block_ms as i64)
                    .arg("STREAMS")
                    .arg(&stream)
                    .arg(">")
                    .query_async(&mut c)
                    .await
            }
            RedisConn::Cluster(arc) => {
                let mut c = arc.lock().await;
                redis::cmd("XREADGROUP")
                    .arg("GROUP")
                    .arg(&group)
                    .arg(&consumer)
                    .arg("COUNT")
                    .arg(1i64)
                    .arg("BLOCK")
                    .arg(block_ms as i64)
                    .arg("STREAMS")
                    .arg(&stream)
                    .arg(">")
                    .query_async(&mut *c)
                    .await
            }
        };

        let raw = match raw {
            Ok(v) => v,
            Err(e) => return EventBusResult::Error(e.to_string()),
        };
        match raw {
            None => EventBusResult::Empty,
            Some(groups) => {
                let Some((_stream, entries)) = groups.into_iter().next() else {
                    return EventBusResult::Empty;
                };
                let Some((stream_id, fields)) = entries.into_iter().next() else {
                    return EventBusResult::Empty;
                };
                let mut payload_str: Option<String> = None;
                for (k, v) in fields {
                    if k == "payload" {
                        payload_str = Some(v);
                        break;
                    }
                }
                let Some(payload_str) = payload_str else {
                    return EventBusResult::Error("missing payload field".into());
                };
                match serde_json::from_str::<EvaluateRequestPayload>(&payload_str) {
                    Ok(payload) => EventBusResult::Consumed { stream_id, payload },
                    Err(e) => EventBusResult::Error(format!("decode payload: {e}")),
                }
            }
        }
    }

    /// Acknowledge that a message has been processed.
    pub async fn ack(&self, stream_id: &str) -> Result<(), redis::RedisError> {
        let stream = self.settings.request_stream.clone();
        let group = self.settings.consumer_group.clone();
        let conn = self.conn();
        match conn {
            RedisConn::Single(mut c) => {
                let _: i64 = redis::cmd("XACK")
                    .arg(&stream)
                    .arg(&group)
                    .arg(stream_id)
                    .query_async(&mut c)
                    .await?;
            }
            RedisConn::Cluster(arc) => {
                let mut c = arc.lock().await;
                let _: i64 = redis::cmd("XACK")
                    .arg(&stream)
                    .arg(&group)
                    .arg(stream_id)
                    .query_async(&mut *c)
                    .await?;
            }
        }
        Ok(())
    }

    /// Claim pending messages idle for longer than `idle_claim_ms`.
    pub async fn claim_idle(&self) -> EventBusResult {
        let consumer = if self.settings.consumer_name.is_empty() {
            Uuid::new_v4().to_string()
        } else {
            self.settings.consumer_name.clone()
        };
        let min_idle = self.settings.idle_claim_ms as i64;
        let stream = self.settings.request_stream.clone();
        let group = self.settings.consumer_group.clone();

        let conn = self.conn();
        let raw: XReadRaw = match conn {
            RedisConn::Single(mut c) => {
                redis::cmd("XAUTOCLAIM")
                    .arg(&stream)
                    .arg(&group)
                    .arg(&consumer)
                    .arg(min_idle)
                    .arg("0-0")
                    .arg("COUNT")
                    .arg(1i64)
                    .query_async(&mut c)
                    .await
            }
            RedisConn::Cluster(arc) => {
                let mut c = arc.lock().await;
                redis::cmd("XAUTOCLAIM")
                    .arg(&stream)
                    .arg(&group)
                    .arg(&consumer)
                    .arg(min_idle)
                    .arg("0-0")
                    .arg("COUNT")
                    .arg(1i64)
                    .query_async(&mut *c)
                    .await
            }
        };
        let raw = match raw {
            Ok(v) => v,
            Err(e) => return EventBusResult::Error(e.to_string()),
        };
        match raw {
            None => EventBusResult::Empty,
            Some(groups) => {
                let Some((_stream, entries)) = groups.into_iter().next() else {
                    return EventBusResult::Empty;
                };
                let Some((stream_id, fields)) = entries.into_iter().next() else {
                    return EventBusResult::Empty;
                };
                let mut payload_str: Option<String> = None;
                for (k, v) in fields {
                    if k == "payload" {
                        payload_str = Some(v);
                        break;
                    }
                }
                let Some(payload_str) = payload_str else {
                    return EventBusResult::Error("missing payload field".into());
                };
                match serde_json::from_str::<EvaluateRequestPayload>(&payload_str) {
                    Ok(payload) => EventBusResult::Consumed { stream_id, payload },
                    Err(e) => EventBusResult::Error(format!("decode payload: {e}")),
                }
            }
        }
    }

    /// Block duration for `tokio::time::timeout` wrapping.
    #[must_use]
    pub fn block_duration(&self) -> Duration {
        Duration::from_millis(self.settings.block_ms as u64)
    }
}
