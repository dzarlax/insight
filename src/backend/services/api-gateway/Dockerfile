# Multi-stage build for Insight API Gateway
#
# Build context: cf/ (parent directory containing both insight/ and cyberfabric-core/)
# Usage: cd /path/to/cf && docker build -f insight/src/backend/services/api-gateway/Dockerfile -t insight-api-gateway:dev .

# Stage 1: Builder
FROM rust:1.92-bookworm AS builder

# Install protobuf-compiler (required by grpc-hub → prost-build)
RUN apt-get update && \
    apt-get install -y --no-install-recommends protobuf-compiler libprotobuf-dev && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Copy cyberfabric-core (dependency)
COPY cyberfabric-core/ ./cyberfabric-core/

# Copy insight backend workspace
COPY insight/src/backend/Cargo.toml insight/src/backend/Cargo.lock ./insight/src/backend/
COPY insight/src/backend/libs/ ./insight/src/backend/libs/
COPY insight/src/backend/plugins/ ./insight/src/backend/plugins/
COPY insight/src/backend/services/api-gateway/ ./insight/src/backend/services/api-gateway/

# Build release binary
WORKDIR /build/insight/src/backend
RUN cargo build --release --bin insight-api-gateway

# Stage 2: Runtime
FROM debian:bookworm-slim

RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy binary
COPY --from=builder /build/insight/src/backend/target/release/insight-api-gateway /app/insight-api-gateway

# Copy config
COPY --from=builder /build/insight/src/backend/services/api-gateway/config/ /app/config/

# Create data dir and non-root user
RUN mkdir -p /app/data && \
    useradd -U -u 1000 appuser && \
    chown -R 1000:1000 /app
USER 1000

EXPOSE 8080

CMD ["/app/insight-api-gateway", "-c", "/app/config/insight.yaml", "run"]
