//! HTTP routes.
//!
//! **P2-API fix**: the HTTP surface now covers the binding spec's engine
//! contract. Endpoints:
//!
//! - `GET /health`
//! - `POST /internal/v1/evaluate` — the stateless evaluate contract
//!   (takes `{account_id, state, rule_pack, tick}`, returns
//!   `{verdict, state_after, metrics}`). This is what the platform's LCC
//!   module calls.
//! - `POST /internal/v1/override` — clear a false-positive breach.
//! - `POST /internal/v1/manual-run` — force re-evaluation of an account.
//! - `GET /internal/v1/breach-report/:account_id` — the trader-facing
//!   "why did I fail" view with evidence (TD-25).
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

use crate::api::handlers::{
    activate_rule_pack, breach_report, create_rule_pack, evaluate_internal,
    evaluate_order, get_account, get_rule_pack, health, manual_run,
    override_breach, supersed_rule_pack, update_rule_pack, SharedState,
};
use axum::{routing::{get, patch, post}, Router};

pub fn router(state: SharedState) -> Router {
    Router::new()
        // Health & readiness.
        .route("/health", get(health))
        // Internal API (platform-side, called by LCC/bridge).
        .route("/internal/v1/evaluate", post(evaluate_internal))
        .route("/internal/v1/override", post(override_breach))
        .route("/internal/v1/manual-run", post(manual_run))
        .route("/internal/v1/breach-report/:account_id", get(breach_report))
        // Public API (tenant-facing).
        .route("/v1/evaluate-order", post(evaluate_order))
        .route("/v1/accounts/:id", get(get_account))
        // Rule-pack CRUD.
        .route("/v1/rule-packs", post(create_rule_pack))
        .route("/v1/rule-packs/:id", get(get_rule_pack))
        .route("/v1/rule-packs/:id", patch(update_rule_pack))
        .route("/v1/rule-packs/:id/activate", post(activate_rule_pack))
        .route("/v1/rule-packs/:id/supersede", post(supersed_rule_pack))
        .with_state(state)
}
