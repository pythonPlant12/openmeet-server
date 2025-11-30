# Stage 1: builder - Compile Rust binary
# Stage 2: prod    - Minimal runtime image with just binary

# --- Stage 1: Builder ---
FROM rust:1.91.1-alpine AS builder

RUN apk add --no-cache musl-dev openssl-dev openssl-libs-static pkgconfig

WORKDIR /app

COPY Cargo.toml Cargo.lock ./

# Create dummy main.rs for dependency compilation
RUN mkdir src && echo "fn main() {}" > src/main.rs

# Build dependencies
RUN cargo build --release && rm -rf src

COPY src ./src

# Build the real app binary
RUN touch src/main.rs && cargo build --release

# --- Stage 2: Production ---
FROM alpine:3.19 AS prod

# Install runtime dependencies
RUN apk add --no-cache ca-certificates

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
