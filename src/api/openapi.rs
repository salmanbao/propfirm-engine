//! OpenAPI spec generation (gated behind the `openapi` cargo feature).
//!
//! When enabled, the server serves:
//! - `GET /openapi.json` — the OpenAPI 3.0 spec (machine-readable)
//! - `GET /swagger-ui/` — interactive Swagger UI (browser)
//!
//! ## Enable
//!
//! ```toml
//! [dependencies]
//! propfirm-engine = { features = ["server", "openapi"] }
//! ```
//!
//! ## Usage
//!
//! Once enabled, the routes are automatically added to the router
//! by `routes::router()`. No configuration needed.
//!
//! The spec documents every endpoint, request/response shape, and
//! error code. The Swagger UI lets you interactively test endpoints
//! without writing curl commands.

#![cfg(feature = "openapi")]

use utoipa::OpenApi;

/// The OpenAPI 3.0 spec for the propfirm-engine HTTP API.
///
/// Served at `GET /openapi.json` when the `openapi` cargo feature is
/// enabled. The Swagger UI is served at `GET /swagger-ui/`.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "Prop Firm Engine API",
        description = "Enterprise-grade risk and rule evaluation engine for proprietary trading firms. Internal service — no authentication.",
        version = "0.2.0",
        license(
            name = "MIT OR Apache-2.0",
            url = "https://github.com/salmanbao/propfirm-engine",
        ),
    ),
    paths(
        // The handler paths are documented inline in src/api/handlers.rs.
        // utoipa picks up #[utoipa::path] attributes from there.
    ),
    tags(
        (name = "internal", description = "Internal API (called by platform backend)"),
        (name = "public", description = "Public API (called by tenant admin tooling)"),
        (name = "health", description = "Health + readiness probes"),
    ),
)]
pub struct ApiDoc;

/// Get the OpenAPI spec as a serde_json::Value (for the /openapi.json route).
pub fn openapi_json() -> serde_json::Value {
    ApiDoc::openapi().to_json().unwrap_or_else(|_| {
        serde_json::json!({
            "error": "failed to serialize OpenAPI spec"
        })
    })
}
