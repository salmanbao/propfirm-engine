//! HTTP server.

use crate::api::handlers::SharedState;
use crate::config::plan::ChallengePlan;
use crate::engine::evaluator::Evaluator;
use crate::notifications::log::LogNotifier;
use crate::persistence::memory::InMemoryStore;
use parking_lot::RwLock;
use std::sync::Arc;

pub struct ServerState {
    pub evaluator: Evaluator,
    pub store: InMemoryStore,
    pub notifier: LogNotifier,
    pub event_store: crate::events::store::EventStore,
}

impl Clone for ServerState {
    /// **P0-A fix**: clone shares the underlying `Arc`ed stores instead
    /// of re-instantiating empty ones. Previously, every
    /// `state.read().clone()` in a handler discarded all in-memory state,
    /// making every endpoint other than `/health` return 404.
    ///
    /// `InMemoryStore`, `LogNotifier`, and `EventStore` are all
    /// `Arc<RwLock<...>>`-backed, so cloning them is cheap (bumps a refcount)
    /// and shares the underlying state — exactly what we want for a
    /// stateful server handler that does `state.read().clone()` to grab a
    /// snapshot of the shared server state.
    fn clone(&self) -> Self {
        ServerState {
            evaluator: self.evaluator.clone(),
            store: self.store.clone(),
            notifier: self.notifier.clone(),
            event_store: self.event_store.clone(),
        }
    }
}

impl ServerState {
    pub fn new(plan: ChallengePlan) -> Self {
        ServerState {
            evaluator: Evaluator::new(plan),
            store: InMemoryStore::new(),
            notifier: LogNotifier::new(),
            event_store: crate::events::store::EventStore::in_memory(),
        }
    }

    pub fn pipeline(&self) -> crate::engine::pipeline::Pipeline<InMemoryStore, LogNotifier> {
        let mut p = crate::engine::pipeline::Pipeline::new(self.evaluator.clone(), self.store.clone(), self.notifier.clone());
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
