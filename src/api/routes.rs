//! HTTP routes.

use crate::api::handlers::{evaluate_order, get_account, health, SharedState};
use axum::{routing::{get, post}, Router};

pub fn router(state: SharedState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/evaluate-order", post(evaluate_order))
        .route("/v1/accounts/:id", get(get_account))
        .with_state(state)
}
