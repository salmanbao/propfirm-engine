//! HTTP routes.
//!
//! **§A.1 fix**: every route is behind [`auth_layer`](crate::api::auth::auth_layer)
//! except `/health` and `/ready` (readiness probes). `/internal/*` routes
//! accept only the service token; `/v1/*` routes accept tenant keys (or
//! the service token). Endpoints:
//!
//! - `GET /health` — liveness (unauthenticated)
//! - `GET /ready` — readiness (unauthenticated)
//! - `POST /internal/v1/evaluate` — the stateless evaluate contract
//!   (takes `{account_id, rule_pack, tick}`, returns
//!   `{verdict, input_hash, metrics}`). This is what the platform's LCC
//!   module calls; **service token required**.
//! - `POST /internal/v1/override` — clear a false-positive breach
//!   (**service token required**).
//! - `POST /internal/v1/manual-run` — force re-evaluation of an account
//!   (**service token required**).
//! - `POST /internal/v1/emergency-stop` — freeze an account immediately
//!   (**service token required**).
//! - `GET /internal/v1/breach-report/:account_id` — the trader-facing
//!   "why did I fail" view with evidence (TD-25) (**service token
//!   required**).
//! - `POST /v1/rule-packs` — create a new rule pack (draft).
//! - `GET /v1/rule-packs/:id` — get a rule pack by id.
//! - `PATCH /v1/rule-packs/:id` — update a draft rule pack.
//! - `POST /v1/rule-packs/:id/activate` — promote draft → active.
//! - `POST /v1/rule-packs/:id/supersede` — mark active → superseded.
//! - `GET /v1/accounts/:id` — get account snapshot.
//! - `POST /v1/evaluate-order` — pre-trade order evaluation.
//!
//! All mutating endpoints accept an `Idempotency-Key` header (the platform
//! treats idempotency as non-negotiable everywhere). The server tracks the
//! last N idempotency keys per endpoint to deduplicate retries.

use crate::api::auth::auth_layer;
use crate::api::handlers::{
    breach_report, create_account, emergency_stop, evaluate_internal, evaluate_order, get_account,
    health, manual_run, override_breach, ready, validate_rule_pack, SharedState,
};
use axum::{
    routing::{get, post},
    Router,
};

pub async fn router(state: SharedState) -> Router {
    let auth = state.read().await.auth.clone();
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/internal/v1/evaluate", post(evaluate_internal))
        .route("/internal/v1/override", post(override_breach))
        .route("/internal/v1/manual-run", post(manual_run))
        .route("/internal/v1/emergency-stop", post(emergency_stop))
        .route("/internal/v1/breach-report/:account_id", get(breach_report))
        .route("/v1/evaluate-order", post(evaluate_order))
        .route("/v1/accounts/:id", get(get_account))
        .route("/v1/accounts", post(create_account))
        // Rule pack validation endpoint (stateless, no storage required)
        .route("/v1/rule-packs/validate", post(validate_rule_pack))
        // §A.1: the auth config rides in the request extensions so the
        // middleware can authenticate each call; the middleware then
        // injects the resolved AuthedIdentity. Layer order matters: the
        // LAST-added layer is outermost, so `Extension` must be added
        // AFTER `from_fn` — the request passes Extension (config inserted)
        // before reaching auth_layer.
        .layer(axum::middleware::from_fn(auth_layer))
        .layer(axum::extract::Extension(auth))
        .with_state(state)
}
