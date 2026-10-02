//! HTTP routes.
//!
//! **No auth layer.** The engine is reached only from the platform
//! backend over the private compose network; trust is established at
//! the network boundary, not in-process.
//!
//! Routes:
//! - `GET /health` — liveness probe
//! - `GET /ready` — readiness probe (verifies idempotency backend is wired)
//! - `GET /metrics` — Prometheus metrics (rendered from the global
//!   PrometheusHandle stored in `ServerState`)
//! - `POST /internal/v1/evaluate` — stateless evaluate contract
//! - `POST /internal/v1/override` — clear a false-positive breach
//! - `POST /internal/v1/manual-run` — force re-evaluation
//! - `POST /internal/v1/emergency-stop` — short-circuit evaluation
//! - `POST /internal/v1/breach-report` — trader-facing "why did I fail"
//! - `POST /v1/evaluate-order` — pre-trade order evaluation
//! - `POST /v1/rule-packs/validate` — validate a rule pack (stateless)
//!
//! Tower middleware layers (outer-to-inner — outermost sees every
//! response, including framework-level rejections like 408 timeout
//! or 413 body-limit):
//! - `http_request_metrics` (axum `from_fn`) — increments the
//!   `propfirm_http_requests_total` counter with `method` + `status`
//!   labels for EVERY response. This is the outermost layer so it
//!   captures timeouts and body-limit rejections that bypass the
//!   handler body.
//! - `TraceLayer` — per-request spans with `method`, `uri`, `request_id`
//! - `TimeoutLayer` — per-request timeout (configurable)
//! - `CompressionLayer` — gzip/brotli response compression
//! - `RequestBodyLimitLayer` — max request body size
//! - `SetRequestIdLayer` — generates `x-request-id` if not present
//! - `PropagateRequestIdLayer` — echoes `x-request-id` back in response

use crate::api::handlers::{
    breach_report, emergency_stop, evaluate_internal, evaluate_order, health, manual_run,
    override_breach, ready, validate_rule_pack, SharedState,
};
use axum::{
    extract::{Request, State},
    middleware::{from_fn, Next},
    response::Response,
    routing::{get, post},
    Router,
};
use std::time::Duration;
use tower_http::compression::CompressionLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

/// Header name used for request id propagation.
const REQUEST_ID_HEADER: &str = "x-request-id";

/// Outermost metrics middleware — increments `propfirm_http_requests_total`
/// with `method` + `status` labels for every response, including the
/// framework-level rejections (body-limit 413, timeout 408, malformed
/// routing 404) that bypass the handler body. Without this layer, the
/// `PropfirmHighErrorRate` Grafana alert (severity: page) has nothing
/// to query — see commit message and `src/api/metrics.rs::record_http_response`
/// for the full history.
async fn http_request_metrics(req: Request, next: Next) -> Response {
    let method = req.method().to_string();
    let resp = next.run(req).await;
    crate::api::metrics::record_http_response(&method, resp.status().as_u16());
    resp
}

/// Build the production router with all middleware layers.
///
/// `body_limit_bytes` and `request_timeout` come from the caller
/// (typically `ServerState` or `Settings`) so they're configurable
/// instead of hard-coded.
pub async fn router_with_limits(
    state: SharedState,
    body_limit_bytes: usize,
    request_timeout: Duration,
) -> Router {
    let r = Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/internal/v1/evaluate", post(evaluate_internal))
        .route("/internal/v1/override", post(override_breach))
        .route("/internal/v1/manual-run", post(manual_run))
        .route("/internal/v1/emergency-stop", post(emergency_stop))
        .route("/internal/v1/breach-report", post(breach_report))
        .route("/v1/evaluate-order", post(evaluate_order))
        .route("/v1/rule-packs/validate", post(validate_rule_pack))
        .route("/metrics", get(metrics_handler));

    // OpenAPI spec + Swagger UI (only when the `openapi` cargo feature is enabled).
    #[cfg(feature = "openapi")]
    let r = r
        .route("/openapi.json", get(openapi_json_handler))
        .route("/swagger-ui", get(swagger_ui_handler))
        .route("/swagger-ui/", get(swagger_ui_handler));

    // Layer order (outermost first). The metrics middleware is added
    // last (making it outermost) so it observes every response —
    // including 408 timeouts and 413 body-limit rejections that the
    // inner layers (TimeoutLayer, RequestBodyLimitLayer) emit without
    // ever running the handler body. Without this placement, the
    // `propfirm_http_requests_total{status=~"5.."}` alert would miss
    // the framework-side failures.
    r.layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
        .layer(
            TraceLayer::new_for_http().make_span_with(|req: &axum::http::Request<_>| {
                let req_id = req
                    .headers()
                    .get(REQUEST_ID_HEADER)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("-")
                    .to_string();
                tracing::info_span!(
                    "http",
                    method = %req.method(),
                    uri = %req.uri(),
                    request_id = %req_id
                )
            }),
        )
        .layer(TimeoutLayer::with_status_code(
            axum::http::StatusCode::REQUEST_TIMEOUT,
            request_timeout,
        ))
        .layer(CompressionLayer::new())
        .layer(RequestBodyLimitLayer::new(body_limit_bytes))
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(from_fn(http_request_metrics))
        .with_state(state)
}

/// Prometheus metrics endpoint.
///
/// Renders the global `PrometheusHandle`'s metrics as a Prometheus-
/// formatted text scrape. The handle is installed at binary startup
/// via `PrometheusBuilder::install_recorder()` (see
/// `src/bin/server.rs::init_metrics`).
pub async fn metrics_handler(
    State(state): State<SharedState>,
) -> Result<String, (axum::http::StatusCode, String)> {
    let s = &state;
    Ok(s.metrics_handle.render())
}

/// `GET /openapi.json` — serves the OpenAPI 3.0 spec.
#[cfg(feature = "openapi")]
pub async fn openapi_json_handler() -> axum::Json<serde_json::Value> {
    axum::Json(crate::api::openapi::openapi_json())
}

/// `GET /swagger-ui/` — serves the Swagger UI HTML page.
#[cfg(feature = "openapi")]
pub async fn swagger_ui_handler() -> axum::response::Html<&'static str> {
    axum::response::Html(SWAGGER_UI_HTML)
}

#[cfg(feature = "openapi")]
const SWAGGER_UI_HTML: &str = r#"<!DOCTYPE html>
<html>
<head>
  <title>Prop Firm Engine — Swagger UI</title>
  <link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5/swagger-ui.css">
</head>
<body>
  <div id="swagger-ui"></div>
  <script src="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5/swagger-ui-bundle.js"></script>
  <script>
    window.onload = function() {
      SwaggerUIBundle({
        url: '/openapi.json',
        dom_id: '#swagger-ui',
        deepLinking: true,
        presets: [SwaggerUIBundle.presets.apis],
        layout: 'BaseLayout',
      });
    };
  </script>
</body>
</html>"#;

/// Backwards-compatible router builder that uses default limits.
/// Calls `router_with_limits` with the hard-coded defaults (2 MiB, 30s).
pub async fn router(state: SharedState) -> Router {
    router_with_limits(state, 2 * 1024 * 1024, Duration::from_secs(30)).await
}
