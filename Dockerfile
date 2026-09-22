FROM rust:1.85-slim-bookworm AS builder

WORKDIR /app

# Install build dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libssl-dev && \
    rm -rf /var/lib/apt/lists/*

# Copy lockfile first for caching
COPY Cargo.lock Cargo.toml ./

# Create a dummy source tree matching every declared target so dependency
# compilation does not fail on missing lib/bin paths.
RUN mkdir -p src/bin && \
    touch src/lib.rs && \
    echo "fn main() {}" > src/bin/server.rs && \
    echo "fn main() {}" > src/bin/cli.rs

# Build dependencies (cached unless lockfile changes)
RUN cargo build --release --features server,tracing --bin propfirm-server --bin propfirm-cli

# Now copy the real source
COPY src ./src
COPY benches ./benches
COPY tests ./tests
COPY examples ./examples

# Build the actual binaries
RUN cargo build --release --features server,tracing --bin propfirm-server --bin propfirm-cli

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates && \
    rm -rf /var/lib/apt/lists/*

RUN useradd -r -u 1000 -s /bin/false propfirm

WORKDIR /app

COPY --from=builder /app/target/release/propfirm-server /app/propfirm-server
COPY --from=builder /app/target/release/propfirm-cli /app/propfirm-cli

RUN chown propfirm:propfirm /app/propfirm-server /app/propfirm-cli && \
    chmod 755 /app/propfirm-server /app/propfirm-cli

USER propfirm

# Default: listen on 0.0.0.0:8080
EXPOSE 8080

ENTRYPOINT ["/app/propfirm-server"]
CMD ["0.0.0.0:8080"]
