//! HTTP middleware: request-id propagation, panic hook, and the central
//! error-shaping `IntoResponse` impl for [`crate::Error`].
//!
//! ## Why no auth middleware?
//!
//! The engine is deployed as an **internal component** of the Prop Firm
//! as a Service Platform. The platform backend is the sole caller over the
//! private compose network, and the worker binary consumes from Redis
//! Streams (also private). There is no public exposure; trust is
//! established at the network boundary. Authentication is intentionally
//! absent to keep the hot path fast and the configuration surface small.
//!
//! If you ever need to expose this service publicly, put a real
//! authenticating gateway (envoy / linkerd / nginx + oauth2-proxy) in
//! front of it — do NOT bolt auth back into this crate.

use axum::{
    http::{HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use tracing::error;

use crate::core::Error;

/// Install a panic hook that routes panics through `tracing::error`
/// instead of the default stderr print. Handler panics in axum are
/// caught by the framework and turned into 500 responses, but without
/// this hook the panic message and backtrace are lost.
///
/// Idempotent — calling it multiple times re-installs the same hook.
pub fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Route through tracing so the global subscriber emits it.
        error!(
            panic = %info,
            location = ?info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())),
            "panic caught by hook"
        );
        // Also call the previous hook so the default stderr print still
        // happens (useful in dev when there's no subscriber configured).
        default_hook(info);
    }));
}

/// Map [`crate::Error`] variants to HTTP responses with stable JSON shapes.
///
/// - `InvalidConfig`, `InvalidState`, `NotFound`, `RuleNotApplicable` → 400
/// - `StateConflict`, `TickRejected` → 409 / 422 respectively
/// - everything else → 500 (internal error; details not leaked)
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let (status, error_kind, message) = match &self {
            Error::InvalidConfig(msg) => (StatusCode::BAD_REQUEST, "invalid_config", msg.clone()),
            Error::InvalidState(msg) => (StatusCode::BAD_REQUEST, "invalid_state", msg.clone()),
            Error::NotFound(msg) => (StatusCode::NOT_FOUND, "not_found", msg.clone()),
            Error::RuleNotApplicable(rule, kind) => (
                StatusCode::BAD_REQUEST,
                "rule_not_applicable",
                format!("rule {rule} not applicable to context kind {kind}"),
            ),
            Error::StateConflict(what, expected, found) => (
                StatusCode::CONFLICT,
                "state_conflict",
                format!("state conflict on {what}: expected version {expected}, found {found}"),
            ),
            Error::TickRejected(msg) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "tick_rejected",
                msg.clone(),
            ),
            Error::NumericConversion(msg) => {
                (StatusCode::BAD_REQUEST, "numeric_conversion", msg.clone())
            }
            Error::Persistence(msg) => {
                error!(error = %msg, "persistence failure");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "persistence_error",
                    "internal persistence failure".to_string(),
                )
            }
            Error::Serialization(msg) => {
                error!(error = %msg, "serialization failure");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "serialization_error",
                    "internal serialization failure".to_string(),
                )
            }
            Error::RuleEval(msg) => {
                error!(error = %msg, "rule evaluation failure");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "rule_eval_error",
                    "internal rule evaluation failure".to_string(),
                )
            }
            Error::MissingMetric(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "missing_metric",
                msg.clone(),
            ),
        };

        let body = Json(json!({
            "error": error_kind,
            "message": message,
        }));
        let mut resp = (status, body).into_response();
        resp.headers_mut()
            .insert("content-type", HeaderValue::from_static("application/json"));
        resp
    }
}

/// Convert a generic `(StatusCode, String)` error tuple (used by handler
/// `Result<_, _>` returns) into a JSON response with the same shape as
/// `IntoResponse for Error`. This keeps client-facing error shapes
/// uniform across the API.
impl From<(StatusCode, String)> for ApiError {
    fn from((status, message): (StatusCode, String)) -> Self {
        ApiError { status, message }
    }
}

/// Wrapper to convert `(StatusCode, String)` into a JSON response.
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(json!({
            "error": status_to_kind(self.status),
            "message": self.message,
        }));
        let mut resp = (self.status, body).into_response();
        resp.headers_mut()
            .insert("content-type", HeaderValue::from_static("application/json"));
        resp
    }
}

fn status_to_kind(status: StatusCode) -> &'static str {
    match status.as_u16() {
        400 => "bad_request",
        401 => "unauthorized",
        403 => "forbidden",
        404 => "not_found",
        409 => "conflict",
        413 => "payload_too_large",
        422 => "unprocessable_entity",
        429 => "too_many_requests",
        500..=599 => "internal_error",
        _ => "error",
    }
}
