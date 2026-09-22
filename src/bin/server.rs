//! HTTP server entry point.
//!
//! **§A.1 fix**: the server is authenticated by default and fails closed.
//! Required environment:
//!
//! - `PROPFIRM_API_KEYS` — comma-separated `tenant-uuid:key` pairs
//!   (one key per tenant).
//! - `PROPFIRM_SERVICE_TOKEN` — the token for the `/internal/*`
//!   bridge/platform-only endpoints (tenant keys are rejected there).
//!
//! Escape hatch (NOT for production): `PROPFIRM_ALLOW_INSECURE=1` runs
//! the server without tenant keys (prints a loud warning);
//! `PROPFIRM_ALLOW_NO_SERVICE_TOKEN=1` allows startup without the
//! internal token.

use std::env;

#[cfg(feature = "server")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let addr = if args.len() > 1 && (args[1] == "--help" || args[1] == "-h") {
        println!("Usage: propfirm-server [ADDR]");
        println!("  ADDR   bind address (default: 0.0.0.0:8080)");
        println!("  --help show this help");
        std::process::exit(0);
    } else if args.len() > 1 {
        args[1].as_str()
    } else {
        "0.0.0.0:8080"
    };
    let plan = propfirm::config::presets::ftmo_phase1();
    println!("Prop Firm Engine HTTP server on http://{addr}");
    propfirm::api::server::run_server(addr, plan).await
}

#[cfg(not(feature = "server"))]
fn main() {
    eprintln!("This binary requires the `server` feature: cargo run --features server --bin propfirm-server");
}
