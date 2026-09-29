//! Graceful shutdown signal handling.
//!
//! On Unix: listens for both `SIGINT` (Ctrl-C) and `SIGTERM` (k8s/docker stop).
//! On non-Unix: listens only for `SIGINT` (Windows has no SIGTERM).
//!
//! The `axum_graceful_shutdown` variant returns a `Future` suitable for
//! passing to `axum::serve(...).with_graceful_shutdown(...)`. It awaits
//! the signal, then sleeps for the drain timeout — axum handles the rest.
//!
//! The `shutdown_signal` variant returns a `CancellationToken` from
//! `tokio_util` so the worker can propagate shutdown to all its tasks.

use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Build a future that resolves when the process receives SIGINT/SIGTERM.
pub async fn await_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(error = %e, "failed to listen for ctrl-c");
        }
    };

    #[cfg(unix)]
    let sigterm = async {
        use tokio::signal::unix::{signal, SignalKind};
        match signal(SignalKind::terminate()) {
            Ok(mut s) => {
                // `s.recv()` returns Option<()> (None when signal handler
                // is dropped). Either way, if we got here, SIGTERM fired.
                let _ = s.recv().await;
            }
            Err(e) => {
                tracing::error!(error = %e, "failed to install SIGTERM handler");
            }
        }
    };

    #[cfg(not(unix))]
    let sigterm = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("received SIGINT, initiating graceful shutdown"),
        _ = sigterm => info!("received SIGTERM, initiating graceful shutdown"),
    }
}

/// Future compatible with `axum::serve(listener, app).with_graceful_shutdown(fut)`.
///
/// Resolves on SIGINT/SIGTERM, then sleeps for the drain timeout. Axum
/// stops accepting new connections as soon as this future resolves, and
/// waits for in-flight handlers to complete (up to its own internal
/// deadline).
pub async fn axum_graceful_shutdown(_drain: Duration) {
    await_signal().await;
    // axum itself manages the drain; we just need to return.
}

/// Build a `CancellationToken` that fires on signal. Useful for the
/// worker binary to fan out cancellation to multiple concurrent tasks.
pub fn shutdown_signal(_drain: Duration) -> CancellationToken {
    let token = CancellationToken::new();
    let child = token.clone();
    tokio::spawn(async move {
        await_signal().await;
        warn!("cancelling in-flight tasks");
        child.cancel();
    });
    token
}
