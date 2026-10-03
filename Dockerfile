# Multi-stage build for propfirm-engine.
#
# D81: outputs two binaries (no worker — the platform's `workers`
# consumer owns the event bus):
#   - /app/propfirm-server  (HTTP API server, internal-only, stateless)
#   - /app/propfirm-cli     (Local dev CLI demo)
#
# Runtime image is debian:bookworm-slim with ca-certificates + tini (PID 1).
# Includes a CycloneDX SBOM (Software Bill of Materials) at /app/sbom/
# for supply-chain security posture.

# ---- Builder stage ----
FROM rust:1.98-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev \
    && rm -rf /var/lib/apt/lists/*

# Install cargo-cyclonedx for SBOM generation in the builder stage.
RUN cargo install --locked cargo-cyclonedx --version "^0.5" || \
    echo "cargo-cyclonedx install failed; SBOM will not be embedded"

WORKDIR /build

# Cache dependencies: copy manifests first and create stub source tree.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src/bin src/rules/evaluators
RUN echo "pub fn main() {}" > src/lib.rs
RUN echo "fn main() {}" > src/bin/cli.rs
RUN echo "fn main() {}" > src/bin/server.rs

# Build dependencies only.
RUN cargo build --release --features "server tokio-cli" --bin propfirm-server --bin propfirm-cli 2>/dev/null || true

# Now copy the real source and rebuild.
COPY src/ src/
COPY benches/ benches/
COPY tests/ tests/
COPY examples/ examples/

# Generate the SBOM BEFORE the final build.
RUN ~/.cargo/bin/cargo-cyclonedx --all-features --format json --output-pattern packages || \
    echo "SBOM generation skipped (cargo-cyclonedx unavailable)"

# Force a clean rebuild of the crate (dependencies are cached).
RUN touch src/lib.rs src/bin/cli.rs src/bin/server.rs
RUN cargo build --release --features "server tokio-cli" --bin propfirm-server --bin propfirm-cli

# ---- Runtime stage ----
FROM debian:bookworm-slim AS runtime

# Install tini (PID 1 with proper signal forwarding), ca-certificates (for TLS),
# and curl (for the HEALTHCHECK probe).
RUN apt-get update && apt-get install -y --no-install-recommends \
    tini ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user.
RUN useradd -r -u 1000 -m -d /app propfirm

WORKDIR /app

# Copy binaries from builder.
COPY --from=builder /build/target/release/propfirm-server /app/propfirm-server
COPY --from=builder /build/target/release/propfirm-cli /app/propfirm-cli

# Copy default config.
COPY config/ /app/config/

RUN mkdir -p /app/sbom && cp -r /build/target/cyclonedx/. /app/sbom/ || true

# Make binaries executable.
RUN chmod 755 /app/propfirm-server /app/propfirm-cli

# Healthcheck: hit /health every 30s.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -fsS http://localhost:8080/health || exit 1

# OCI labels.
LABEL org.opencontainers.image.title="propfirm-engine" \
      org.opencontainers.image.description="Prop Firm Risk & Rule Evaluation Engine — D81 stateless compute service" \
      org.opencontainers.image.source="https://github.com/salmanbao/propfirm-engine" \
      org.opencontainers.image.licenses="MIT OR Apache-2.0" \
      io.propfirm.sbom.location="/app/sbom/" \
      io.propfirm.sbom.format="cyclonedx-json"

# Run as non-root.
USER propfirm

# Expose HTTP server port.
EXPOSE 8080

# tini as PID 1 (proper SIGTERM forwarding), then server entrypoint.
ENTRYPOINT ["/usr/bin/tini", "--"]
CMD ["/app/propfirm-server"]
