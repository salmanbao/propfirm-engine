//! HTTP server.
//!
//! **§A.1 fix**: authentication is now real. The old `ServerState.api_key:
//! Option<String>` was never set from any environment or config source and
//! the old middleware short-circuited on `None` — the shipped server was
//! unauthenticated behind auth-looking scaffolding. Credentials now come
//! from the environment at startup, fail closed (see
//! [`AuthConfig::from_env`]), and the middleware resolves a per-tenant
//! identity from the presented key.

use crate::api::auth::AuthConfig;
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
    /// **§A.1 fix**: parsed auth configuration (per-tenant keys + service
    /// token). Required to build the router; an unauthenticated server
    /// needs an explicit `PROPFIRM_ALLOW_INSECURE=1` escape hatch.
    pub auth: AuthConfig,
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
            auth: self.auth.clone(),
        }
    }
}

impl ServerState {
    #[must_use]
    pub fn new(plan: ChallengePlan, auth: AuthConfig) -> Self {
        ServerState {
            evaluator: Evaluator::new(&plan),
            store: InMemoryStore::new(),
            notifier: LogNotifier::new(),
            event_store: crate::events::store::EventStore::in_memory(),
            rule_pack_store: InMemoryRulePackStore::new(),
            idempotency: IdempotencyStore::with_defaults(),
            auth,
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

/// Builds and runs the HTTP server. **Fail closed**: refuses to start
/// when no credentials are configured unless the explicit insecure
/// escape hatch is set (a loud warning is printed in that case).
pub async fn run_server(addr: &str, plan: ChallengePlan) -> Result<(), Box<dyn std::error::Error>> {
    let auth = AuthConfig::from_env()?;
    for warning in auth.insecure_warnings() {
        eprintln!("{warning}");
    }
    run_server_with_auth(addr, plan, auth).await
}

/// Runs the server with an explicit [`AuthConfig`] (used by tests and by
/// embedders that build configuration themselves).
pub async fn run_server_with_auth(
    addr: &str,
    plan: ChallengePlan,
    auth: AuthConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let state: SharedState = Arc::new(RwLock::new(ServerState::new(plan, auth)));
    let app = crate::api::routes::router(state);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("propfirm-engine HTTP server listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
