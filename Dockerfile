# Stage 1: builder - Compile Rust binary
# Stage 2: prod    - Minimal runtime image with just binary

# --- Stage 1: Builder ---
FROM rust:1.91.1-bookworm AS builder

# Install build dependencies including PostgreSQL client library
RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config \
    libssl-dev \
    libpq-dev \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY Cargo.toml Cargo.lock ./

# Create dummy main.rs for dependency compilation
RUN mkdir src && echo "fn main() {}" > src/main.rs

# Build dependencies (without migrations yet)
RUN cargo build --release && rm -rf src

# Copy source and migrations (needed for embed_migrations! macro)
COPY src ./src
COPY migrations ./migrations
COPY diesel.toml ./

# Build the real app binary
RUN touch src/main.rs && cargo build --release

# --- Stage 2: Production ---
FROM debian:bookworm-slim AS prod

# Install runtime dependencies (libpq for PostgreSQL, ca-certificates for HTTPS, netcat for healthcheck)
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libpq5 \
    netcat-openbsd \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy binary from builder
COPY --from=builder /app/target/release/openmeet-server /app/openmeet-server

# Copy certs directory (will be mounted in production)
RUN mkdir -p /app/certs

# Expose WebSocket port
EXPOSE 8081

# Health check via TCP (WebSocket server)
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s \
    CMD nc -z localhost 8081 || exit 1

# Run the server
CMD ["/app/openmeet-server"]
