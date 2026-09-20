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
}

impl Clone for ServerState {
    fn clone(&self) -> Self {
        ServerState {
            evaluator: self.evaluator.clone(),
            store: InMemoryStore::new(),
            notifier: LogNotifier::new(),
        }
    }
}

impl ServerState {
    pub fn new(plan: ChallengePlan) -> Self {
        ServerState {
            evaluator: Evaluator::new(plan),
            store: InMemoryStore::new(),
            notifier: LogNotifier::new(),
        }
    }

    pub fn pipeline(&self) -> crate::engine::pipeline::Pipeline<InMemoryStore, LogNotifier> {
        crate::engine::pipeline::Pipeline::new(self.evaluator.clone(), self.store.clone(), self.notifier.clone())
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
