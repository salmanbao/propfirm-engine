//! HTTP server entry point.

#[cfg(feature = "server")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let plan = propfirm::config::presets::ftmo_phase1();
    let addr = "0.0.0.0:8080";
    println!("Prop Firm Engine HTTP server on http://{addr}");
    propfirm::api::server::run_server(addr, plan).await
}

#[cfg(not(feature = "server"))]
fn main() {
    eprintln!("This binary requires the `server` feature: cargo run --features server --bin propfirm-server");
}
