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

use crate::persistence::redis_store::{io_error_to_redis, RedisConn};
use crate::settings::EventBusSettings;

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
    Error {
        /// Human-readable error message.
        message: String,
        /// Stream ID if the error happened after a message was read
        /// (e.g. decode failure). Used to ACK poison messages so they
        /// don't loop forever in XAUTOCLAIM.
        stream_id: Option<String>,
    },
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

    /// Clone the underlying connection.
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
        match self.conn() {
            RedisConn::Single { mut producer, .. } => {
                let _: () = redis::cmd("XGROUP")
                    .arg("CREATE")
                    .arg(&stream)
                    .arg(&group)
                    .arg(start_id)
                    .arg("MKSTREAM")
                    .query_async(&mut producer)
                    .await
                    .unwrap_or(());
            }
            RedisConn::Cluster(pool) => {
                let mut conn = pool.get().await.map_err(io_error_to_redis)?;
                let _: () = redis::cmd("XGROUP")
                    .arg("CREATE")
                    .arg(&stream)
                    .arg(&group)
                    .arg(start_id)
                    .arg("MKSTREAM")
                    .query_async(&mut *conn)
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
        match self.conn() {
            RedisConn::Single { mut producer, .. } => {
                let id: String = redis::cmd("XADD")
                    .arg(&stream)
                    .arg("*")
                    .arg("request_id")
                    .arg(&payload.request_id)
                    .arg("payload")
                    .arg(&serialized)
                    .query_async(&mut producer)
                    .await?;
                Ok(id)
            }
            RedisConn::Cluster(pool) => {
                let mut conn = pool.get().await.map_err(io_error_to_redis)?;
                let id: String = redis::cmd("XADD")
                    .arg(&stream)
                    .arg("*")
                    .arg("request_id")
                    .arg(&payload.request_id)
                    .arg("payload")
                    .arg(&serialized)
                    .query_async(&mut *conn)
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
        match self.conn() {
            RedisConn::Single { mut producer, .. } => {
                let id: String = redis::cmd("XADD")
                    .arg(&stream)
                    .arg("*")
                    .arg("request_id")
                    .arg(&payload.request_id)
                    .arg("payload")
                    .arg(&serialized)
                    .query_async(&mut producer)
                    .await?;
                Ok(id)
            }
            RedisConn::Cluster(pool) => {
                let mut conn = pool.get().await.map_err(io_error_to_redis)?;
                let id: String = redis::cmd("XADD")
                    .arg(&stream)
                    .arg("*")
                    .arg("request_id")
                    .arg(&payload.request_id)
                    .arg("payload")
                    .arg(&serialized)
                    .query_async(&mut *conn)
                    .await?;
                Ok(id)
            }
        }
    }

    /// Consume one batch of messages from the request stream via the
    /// consumer group. Blocks for `block_ms` if no messages are available.
    ///
    /// The `consumer_name` parameter is used as the Redis consumer
    /// identity for XREADGROUP. This should be a stable per-pod
    /// identifier (e.g., the pod name) so that PEL entries are
    /// attributed to the right consumer for debugging.
    pub async fn consume_request(&self, consumer_name: &str) -> EventBusResult {
        // Use the provided consumer_name, falling back to the
        // settings.consumer_name if the caller passes an empty
        // string, and finally to a stable hash of the process ID
        // (NOT a random UUID per call — that caused PEL
        // fragmentation where every unacked message was tied to
        // a unique consumer name).
        let consumer = if !consumer_name.is_empty() {
            consumer_name.to_string()
        } else if !self.settings.consumer_name.is_empty() {
            self.settings.consumer_name.clone()
        } else {
            // Fallback: use a stable per-process identifier.
            // This is still not ideal (multiple tasks in the
            // same process share the same consumer name), but
            // it's far better than a fresh UUID per call.
            format!("pid-{}", std::process::id())
        };
        let block_ms = self.settings.block_ms;
        let group = self.settings.consumer_group.clone();
        let stream = self.settings.request_stream.clone();

        let raw: redis::RedisResult<Option<redis::Value>> = match self.conn() {
            RedisConn::Single {
                consumer: mut consumer_conn,
                ..
            } => {
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
                    .query_async(&mut consumer_conn)
                    .await
            }
            RedisConn::Cluster(pool) => {
                let mut conn = match pool.get().await {
                    Ok(c) => c,
                    Err(e) => {
                        return EventBusResult::Error {
                            message: io_error_to_redis(e).to_string(),
                            stream_id: None,
                        }
                    }
                };
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
                    .query_async(&mut *conn)
                    .await
            }
        };

        let raw = match raw {
            Ok(v) => v,
            Err(e) => {
                return EventBusResult::Error {
                    message: e.to_string(),
                    stream_id: None,
                }
            }
        };
        let Some(redis::Value::Array(streams)) = raw else {
            return EventBusResult::Empty;
        };
        let Some(redis::Value::Array(first_stream)) = streams.into_iter().next() else {
            return EventBusResult::Empty;
        };
        let mut stream_iter = first_stream.into_iter();
        let Some(_stream_name) = stream_iter.next().and_then(value_as_string) else {
            return EventBusResult::Empty;
        };
        let Some(redis::Value::Array(entries)) = stream_iter.next() else {
            return EventBusResult::Empty;
        };
        let Some(redis::Value::Array(first_entry)) = entries.into_iter().next() else {
            return EventBusResult::Empty;
        };
        let mut entry_iter = first_entry.into_iter();
        let Some(stream_id) = entry_iter.next().and_then(value_as_string) else {
            return EventBusResult::Empty;
        };
        let Some(redis::Value::Array(fields)) = entry_iter.next() else {
            return EventBusResult::Empty;
        };
        let mut payload_str: Option<String> = None;
        let mut field_iter = fields.into_iter();
        while let Some(k) = field_iter.next().and_then(value_as_string) {
            if let Some(v) = field_iter.next() {
                if k == "payload" {
                    payload_str = value_as_string(v);
                    break;
                }
            }
        }
        let Some(payload_str) = payload_str else {
            return EventBusResult::Error {
                message: "missing payload field".into(),
                stream_id: Some(stream_id),
            };
        };
        match serde_json::from_str::<EvaluateRequestPayload>(&payload_str) {
            Ok(payload) => EventBusResult::Consumed { stream_id, payload },
            Err(e) => {
                // ACK poison messages so they don't get re-delivered
                // forever by XAUTOCLAIM.
                let _ = self.ack(&stream_id).await;
                EventBusResult::Error {
                    message: format!("decode payload: {e}"),
                    stream_id: Some(stream_id),
                }
            }
        }
    }

    /// Acknowledge that a message has been processed.
    pub async fn ack(&self, stream_id: &str) -> Result<(), redis::RedisError> {
        let stream = self.settings.request_stream.clone();
        let group = self.settings.consumer_group.clone();
        match self.conn() {
            RedisConn::Single { mut producer, .. } => {
                let _: i64 = redis::cmd("XACK")
                    .arg(&stream)
                    .arg(&group)
                    .arg(stream_id)
                    .query_async(&mut producer)
                    .await?;
            }
            RedisConn::Cluster(pool) => {
                let mut conn = pool.get().await.map_err(io_error_to_redis)?;
                let _: i64 = redis::cmd("XACK")
                    .arg(&stream)
                    .arg(&group)
                    .arg(stream_id)
                    .query_async(&mut *conn)
                    .await?;
            }
        }
        Ok(())
    }

    /// Claim pending messages idle for longer than `idle_claim_ms`.
    ///
    /// The `consumer_name` parameter is used as the Redis consumer
    /// identity for XAUTOCLAIM. Should be stable per-pod (same as
    /// `consume_request`).
    pub async fn claim_idle(&self, consumer_name: &str) -> EventBusResult {
        let consumer = if !consumer_name.is_empty() {
            consumer_name.to_string()
        } else if !self.settings.consumer_name.is_empty() {
            self.settings.consumer_name.clone()
        } else {
            format!("pid-{}-recovery", std::process::id())
        };
        let min_idle = self.settings.idle_claim_ms as i64;
        let stream = self.settings.request_stream.clone();
        let group = self.settings.consumer_group.clone();

        #[allow(clippy::type_complexity)]
        let raw: redis::RedisResult<
            Option<(String, Vec<(String, Vec<(String, String)>)>, Vec<String>)>,
        > = match self.conn() {
            RedisConn::Single {
                consumer: mut consumer_conn,
                ..
            } => {
                redis::cmd("XAUTOCLAIM")
                    .arg(&stream)
                    .arg(&group)
                    .arg(&consumer)
                    .arg(min_idle)
                    .arg("0-0")
                    .arg("COUNT")
                    .arg(10i64)
                    .query_async(&mut consumer_conn)
                    .await
            }
            RedisConn::Cluster(pool) => {
                let mut conn = match pool.get().await {
                    Ok(c) => c,
                    Err(e) => {
                        return EventBusResult::Error {
                            message: io_error_to_redis(e).to_string(),
                            stream_id: None,
                        }
                    }
                };
                redis::cmd("XAUTOCLAIM")
                    .arg(&stream)
                    .arg(&group)
                    .arg(&consumer)
                    .arg(min_idle)
                    .arg("0-0")
                    .arg("COUNT")
                    .arg(10i64)
                    .query_async(&mut *conn)
                    .await
            }
        };
        let raw = match raw {
            Ok(v) => v,
            Err(e) => {
                return EventBusResult::Error {
                    message: e.to_string(),
                    stream_id: None,
                }
            }
        };
        match raw {
            None => EventBusResult::Empty,
            Some((_next_id, entries, _deleted)) => {
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
                    return EventBusResult::Error {
                        message: "missing payload field".into(),
                        stream_id: Some(stream_id),
                    };
                };
                match serde_json::from_str::<EvaluateRequestPayload>(&payload_str) {
                    Ok(payload) => EventBusResult::Consumed { stream_id, payload },
                    Err(e) => {
                        let _ = self.ack(&stream_id).await;
                        EventBusResult::Error {
                            message: format!("decode payload: {e}"),
                            stream_id: Some(stream_id),
                        }
                    }
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

/// Extract a String from a redis::Value (BulkString or SimpleString).
fn value_as_string(v: redis::Value) -> Option<String> {
    match v {
        redis::Value::BulkString(b) => Some(String::from_utf8_lossy(&b).to_string()),
        redis::Value::SimpleString(s) => Some(s),
        _ => None,
    }
}
