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
use crate::api::idempotency::{IdempotencyBackend, IdempotencyStore};
use crate::notifications::log::LogNotifier;
use std::sync::Arc;
use tokio::sync::RwLock;

pub struct ServerState {
    pub notifier: LogNotifier,
    pub idempotency: Arc<dyn IdempotencyBackend>,
    /// **§A.1 fix**: parsed auth configuration (per-tenant keys + service
    /// token. Required to build the router; an unauthenticated server
    /// needs an explicit `PROPFIRM_ALLOW_INSECURE=1` escape hatch.
    pub auth: AuthConfig,
}

impl Clone for ServerState {
    /// **P0-A fix**: clone shares the underlying `Arc`ed stores instead
    /// of re-instantiating empty ones. Previously, every
    /// `state.read().clone()` in a handler discarded all in-memory state,
    /// making every endpoint other than `/health` return 404.
    ///
    fn clone(&self) -> Self {
        ServerState {
            notifier: self.notifier.clone(),
            idempotency: self.idempotency.clone(),
            auth: self.auth.clone(),
        }
    }
}

impl ServerState {
    #[must_use]
    pub fn new(auth: AuthConfig) -> Self {
        ServerState {
            notifier: LogNotifier::new(),
            idempotency: Arc::new(IdempotencyStore::with_defaults()),
            auth,
        }
    }

    #[must_use]
    pub fn pipeline(
        &self,
    ) -> crate::engine::pipeline::Pipeline<crate::notifications::log::LogNotifier> {
        crate::engine::pipeline::Pipeline::new(
            crate::engine::evaluator::Evaluator::with_registry(
                crate::rules::registry::RuleRegistry::with_default_rules(),
            ),
            self.notifier.clone(),
        )
    }
}

/// Builds and runs the HTTP server. **Fail closed**: refuses to start
/// when no credentials are configured unless the explicit insecure
/// escape hatch is set (a loud warning is printed in that case).
pub async fn run_server(addr: &str) -> Result<(), Box<dyn std::error::Error>> {
    let auth = AuthConfig::from_env()?;
    for warning in auth.insecure_warnings() {
        eprintln!("{warning}");
    }
    run_server_with_auth(addr, auth).await
}

/// Runs the server with an explicit [`AuthConfig`] (used by tests and by
/// embedders that build configuration themselves).
pub async fn run_server_with_auth(
    addr: &str,
    auth: AuthConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let state = ServerState::new(auth);

    let state: SharedState = Arc::new(RwLock::new(state));
    let app = crate::api::routes::router(state).await;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("propfirm-engine HTTP server listening on {addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
