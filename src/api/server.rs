//! HTTP server.

use crate::api::handlers::SharedState;
use crate::api::idempotency::IdempotencyStore;
use crate::config::plan::ChallengePlan;
use crate::engine::evaluator::Evaluator;
use crate::notifications::log::LogNotifier;
use crate::persistence::memory::InMemoryStore;
use crate::persistence::rulepack_store::InMemoryRulePackStore;
use parking_lot::RwLock;
use std::sync::Arc;

pub struct ServerState {
    pub evaluator: Evaluator,
    pub store: InMemoryStore,
    pub notifier: LogNotifier,
    pub event_store: crate::events::store::EventStore,
    pub rule_pack_store: InMemoryRulePackStore,
    pub idempotency: IdempotencyStore,
    pub api_key: Option<String>,
}

impl Clone for ServerState {
    /// **P0-A fix**: clone shares the underlying `Arc`ed stores instead
    /// of re-instantiating empty ones. Previously, every
    /// `state.read().clone()` in a handler discarded all in-memory state,
    /// making every endpoint other than `/health` return 404.
    ///
    /// `InMemoryStore`, `LogNotifier`, `EventStore`,
    /// `InMemoryRulePackStore`, and `IdempotencyStore` are all
    /// `Arc`-backed, so cloning them is cheap (bumps a refcount)
    /// and shares the underlying state.
    fn clone(&self) -> Self {
        ServerState {
            evaluator: self.evaluator.clone(),
            store: self.store.clone(),
            notifier: self.notifier.clone(),
            event_store: self.event_store.clone(),
            rule_pack_store: self.rule_pack_store.clone(),
            idempotency: self.idempotency.clone(),
            api_key: self.api_key.clone(),
        }
    }
}

impl ServerState {
    #[must_use]
    pub fn new(plan: ChallengePlan) -> Self {
        ServerState {
            evaluator: Evaluator::new(&plan),
            store: InMemoryStore::new(),
            notifier: LogNotifier::new(),
            event_store: crate::events::store::EventStore::in_memory(),
            rule_pack_store: InMemoryRulePackStore::new(),
            idempotency: IdempotencyStore::with_defaults(),
            api_key: None,
        }
    }

    #[must_use]
    pub fn pipeline(&self) -> crate::engine::pipeline::Pipeline<InMemoryStore, LogNotifier> {
        let mut p = crate::engine::pipeline::Pipeline::new(
            self.evaluator.clone(),
            self.store.clone(),
            self.notifier.clone(),
        );
        // Replace the pipeline's default event store with our shared one.
        p.event_store = self.event_store.clone();
        p
    }
}

pub async fn run_server(addr: &str, plan: ChallengePlan) -> Result<(), Box<dyn std::error::Error>> {
    let state: SharedState = Arc::new(RwLock::new(ServerState::new(plan)));
    let app = crate::api::routes::router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("propfirm-engine HTTP server listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}

pub async fn auth_layer(
    req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> Result<axum::http::Response<axum::body::Body>, axum::http::StatusCode> {
    let expected = match req
        .extensions()
        .get::<Option<String>>()
        .and_then(|k| k.as_ref())
    {
        Some(k) => k.clone(),
        None => return Ok(next.run(req).await),
    };
    let auth_header = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or(axum::http::StatusCode::UNAUTHORIZED)?;
    if auth_header != format!("Bearer {expected}") {
        return Err(axum::http::StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(req).await)
}
