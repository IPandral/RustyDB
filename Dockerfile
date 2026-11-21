# RustyDB Docker Image
# Multi-stage build for minimal image size

# ============================================================================
# Stage 1: Build
# ============================================================================
FROM rust:bookworm AS builder

# Install build dependencies
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY benches ./benches

RUN cargo build --locked --release --features server

# ============================================================================
# Stage 2: Runtime
# ============================================================================
FROM debian:bookworm-slim AS runtime

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl \
    && rm -rf /var/lib/apt/lists/*

# Create non-root user for security
RUN useradd -m -u 1000 -s /bin/bash rustydb

# Create data directory
RUN mkdir -p /data && chown rustydb:rustydb /data

WORKDIR /app

# Copy binary from builder
COPY --from=builder /app/target/release/rustydb /app/rustydb

# Set ownership
RUN chown -R rustydb:rustydb /app

# Switch to non-root user
USER rustydb

# ============================================================================
# Configuration via Environment Variables
# ============================================================================
# RUSTYDB_HOST        - Bind address (default: 0.0.0.0)
# RUSTYDB_PORT        - HTTP API port (default: 8080)
# RUSTYDB_WIRE_PORT   - MySQL wire protocol port (default: 3307)
# RUSTYDB_USERNAME    - Auth username (shared by HTTP and wire protocol)
# RUSTYDB_PASSWORD    - Auth password (shared by HTTP and wire protocol)
# RUSTYDB_DATA_DIR    - Data directory (default: /data)
# RUSTYDB_MEMORY_ONLY - Use in-memory only mode (default: false)
# RUSTYDB_PLAN_CACHE_CAPACITY - Parsed/optimized plan LRU entries (default: 256)
# RUSTYDB_MEMORY_POOL_CAPACITY - Reusable SQL scratch buffers (default: 32)
# RUSTYDB_BLOOM_FALSE_POSITIVE_RATE - Per-index Bloom target (default: 0.01)

ENV RUSTYDB_HOST=0.0.0.0
ENV RUSTYDB_PORT=8080
ENV RUSTYDB_WIRE_PORT=3307
ENV RUSTYDB_DATA_DIR=/data
ENV RUSTYDB_MEMORY_ONLY=false
ENV RUSTYDB_PLAN_CACHE_CAPACITY=256
ENV RUSTYDB_MEMORY_POOL_CAPACITY=32
ENV RUSTYDB_BLOOM_FALSE_POSITIVE_RATE=0.01

# Expose default ports (HTTP API + MySQL wire protocol)
EXPOSE 8080
EXPOSE 3307

# Health check
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:${RUSTYDB_PORT}/health || exit 1

# Volume for persistent data
VOLUME ["/data"]

# Run the server
ENTRYPOINT ["/app/rustydb"]
CMD ["--server"]
