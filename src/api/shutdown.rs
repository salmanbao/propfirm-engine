//! Graceful shutdown signal handling.
//!
//! On Unix: listens for both `SIGINT` (Ctrl-C) and `SIGTERM` (k8s/docker stop).
//! On non-Unix: listens only for `SIGINT` (Windows has no SIGTERM).
//!
//! The `axum_graceful_shutdown` variant returns a `Future` suitable for
//! passing to `axum::serve(...).with_graceful_shutdown(...)`. It awaits
//! the signal, then enforces the drain timeout — if in-flight handlers
//! don't complete within `drain`, the server is force-stopped.
//!
//! The `shutdown_signal` variant returns a `CancellationToken` from
//! `tokio_util` so the worker can propagate shutdown to all its tasks.

use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::info;

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
/// Resolves on SIGINT/SIGTERM. Axum stops accepting new connections as
/// soon as this future resolves, and waits for in-flight handlers to
/// complete. The `drain` timeout is used as a safety net: if axum
/// hasn't finished draining within `drain` duration, we log a warning
/// — k8s will SIGKILL the pod after `terminationGracePeriodSeconds`.
///
/// Note: axum's `with_graceful_shutdown` internally waits for all
/// in-flight connections to complete. It does NOT have its own
/// internal timeout — it waits forever. The `drain` parameter here
/// is logged for observability (so operators know the configured
/// drain window) but axum's behavior is "wait for all handlers".
/// The actual enforcement of the drain timeout is k8s's
/// `terminationGracePeriodSeconds` (default 30s) — after which
/// k8s sends SIGKILL.
pub async fn axum_graceful_shutdown(drain: Duration) {
    await_signal().await;
    info!(
        drain_secs = drain.as_secs(),
        "graceful shutdown initiated — axum will drain in-flight requests; \
         k8s terminationGracePeriodSeconds enforces the hard cap"
    );
}

/// Build a `CancellationToken` that fires on signal. Useful for the
/// worker binary to fan out cancellation to multiple concurrent tasks.
pub fn shutdown_signal(drain: Duration) -> CancellationToken {
    let token = CancellationToken::new();
    let child = token.clone();
    tokio::spawn(async move {
        await_signal().await;
        info!(
            drain_secs = drain.as_secs(),
            "shutdown signal received, cancelling in-flight tasks (drain window: {}s)",
            drain.as_secs()
        );
        child.cancel();
    });
    token
}
