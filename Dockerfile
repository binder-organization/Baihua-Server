# =============================================================================
# Baihua Server - Production Docker Image
# =============================================================================
# Build:   docker build -t baihua-server .
# Run:     docker run -e JWT_SECRET=... -e POSTGRES_HOST=database ... baihua-server
# Compose: docker compose --profile production up -d
#
# Multi-platform build (e.g. build AMD64 on ARM Mac):
#   docker buildx build --platform linux/amd64 -t baihua-server .
# =============================================================================

# Stage 1: Build ---------------------------------------------------------------
# Use BUILDPLATFORM so the builder runs on the host CPU architecture.
FROM --platform=$BUILDPLATFORM rust:1.88-slim-bookworm AS builder
ARG TARGETPLATFORM
WORKDIR /app

# Install build-time system dependencies
#   libssl-dev, pkg-config — required by sqlx (TLS) and jsonwebtoken
RUN apt-get update \
    && apt-get install -y --no-install-recommends libssl-dev pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Copy the entire project and build the release binary
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY migrations/ migrations/
RUN cargo build --release --locked

# Stage 2: Runtime ------------------------------------------------------------
FROM debian:bookworm-slim
WORKDIR /app

# Create a non-root user for running the server process.
# Security: reduces blast radius of potential container escapes.
RUN groupadd -r baihua && useradd -r -g baihua -d /app -s /sbin/nologin baihua

# Install runtime system dependencies
#   ca-certificates — TLS verification for outbound HTTPS (GitHub API, etc.)
#   curl            — HEALTHCHECK probe
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

# Binary
COPY --from=builder /app/target/release/baihua-server .
# SQL migrations — loaded at startup by sqlx::migrate::Migrator
# (looks for ./migrations/ relative to the binary in production mode)
COPY --from=builder /app/migrations/ migrations/

# Pre-create the app data directory. The server auto-generates a
# production-ready config.toml on first startup via load_or_create_profile().
RUN mkdir -p /app/.baihua/logs /app/.baihua/avatars \
    && chown -R baihua:baihua /app

EXPOSE 2424

# ---------------------------------------------------------------------------
# HEALTHCHECK
# Verifies both the server process AND database connectivity.
# /health (src/health.rs) executes "SELECT 1" against PostgreSQL.
#
# Note: startup order is gated by docker-compose (database → service_healthy → server).
# For standalone runs, ensure the database is reachable before this container starts.
# ---------------------------------------------------------------------------
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD curl -f http://localhost:2424/health

# Drop privileges before starting the server
USER baihua

# ---------------------------------------------------------------------------
# REQUIRED RUNTIME ENVIRONMENT VARIABLES
# None of these are set in the image — they MUST be provided at runtime:
#
#   BAIHUA_ENV        Set "production" for production mode (JSON logs, no console)
#   JWT_SECRET        Long random string (≥256 bits). MANDATORY in production.
#   POSTGRES_USER     Database user
#   POSTGRES_PASSWORD Database password
#   POSTGRES_HOST     Database hostname (use "database" when running with docker-compose)
#   POSTGRES_PORT     Database port (use "5432" in container networks)
#   POSTGRES_DB       Database name
#
# See .env.example for defaults and docker-compose.yml for production setup.
# ---------------------------------------------------------------------------
CMD ["./baihua-server"]
