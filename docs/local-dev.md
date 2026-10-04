# Local Development & Verification Guide (D81 Stateless Design)

**Important**: As of v0.2.0, the propfirm-engine follows the D81 stateless compute service design.
The engine is a pure function that transforms inputs to outputs without retaining any server-side state.
All persistence concerns (idempotency, event storage, audit trail, state storage) are handled by the platform.

This guide walks you through bringing up the propfirm-engine locally for development and verification.
Since the engine is now stateless, you can run it without external dependencies (Postgres, Redis) for many tasks.

## Prerequisites

- Rust toolchain (for building from source)
- `curl`, `jq` (optional, for HTTP API testing)
- (Optional) Docker if you want to test with external services, but not required for engine itself

## Quick start (stateless engine)

The engine can be run as a library or as an HTTP server without any external databases.

### Running as a library

```bash
# Create a new Rust project for testing
cargo new propfirm-test --bin
cd propfirm-test
```

Add propfirm-engine as a dependency in `Cargo.toml`:

```toml
[dependencies]
propfirm-engine = { version = "0.2", features = ["serialization"] }
```

Then run `cargo run` after adding your test code (see examples below).

### Running as HTTP server (no persistence needed)

```bash
# Clone the repository if you haven't already
git clone https://github.com/salmanbao/propfirm-engine
cd propfirm-engine

# Build and run the server (only server feature needed)
cargo run --release --features server --bin propfirm-server
```

The server will bind to `0.0.0.0:8080` by default. Since persistence is platform responsibility,
the server will use in-memory implementations for idempotency and event store (suitable for dev/testing).

## Verification checklist

### 1. Server is up and responding

```bash
curl -fsS http://localhost:8080/health
# Expected: ok

curl -fsS http://localhost:8080/ready
# Expected: ready
```

### 2. Prometheus metrics

```bash
curl -fsS http://localhost:8080/metrics | head -10
# Expected: Prometheus-formatted metrics with propfirm_* prefix
```

### 3. End-to-end evaluation (HTTP path)

```bash
# Generate IDs for tenant and account
TENANT_ID=$(uuidgen)
ACCOUNT_ID=$(uuidgen)

# Build a simple evaluation request (using FTMO Phase 1 preset as example)
# For brevity, we'll use a minimal account_state - in practice you'd use a proper preset
cat > /tmp/eval.json <<EOF
{
  "account_id": "$ACCOUNT_ID",
  "account_state": {
    "id": "$ACCOUNT_ID",
    "tenant_id": "$TENANT_ID",
    "initial_balance": { "amount": "100000", "code": "USD" },
    "balance": { "amount": "100000", "code": "USD" },
    "equity": { "amount": "100000", "code": "USD" },
    "status": "Active",
    "plan": {
      "id": "ftmo-phase-1",
      "name": "FTMO Phase 1",
      "tenant_id": "$TENANT_ID",
      "initial_balance": { "amount": "100000", "code": "USD" },
      "drawdown_max_relative": 0.1,
      "drawdown_max_absolute": 0.0,
      "profit_target_relative": 0.08,
      "enabled_rules": [
        "max_drawdown", "profit_target", "max_total_lots", "margin", "trading_hours"
      ],
      "max_total_lots": 10,
      "leverage": 100,
      "trading_hours": { "start": 0, "end": 23 },
      "timezone": "UTC"
    },
    "version": 1
  },
  "bridge_tick": {
    "payload": {
      "symbol": "EURUSD",
      "bid": 1.0850,
      "ask": 1.0852,
      "time": $(date +%s)000,
      "equity": 100000,
      "balance": 100000
    }
  }
}
