# Multi-stage, multi-arch build for the Chronos server plus the static frontend.
#
# Runtime data lives under a single writable data directory (/data). The CAIDA
# AS-relationship dataset is downloaded and cached there automatically at
# startup, so no dataset needs to be mounted for the happy path. Optionally,
# operators may mount their own files and point the matching environment
# variables at them:
#   CHRONOS_GEOLITE2_CITY_DB, CHRONOS_GEOLITE2_ASN_DB, CHRONOS_CAIDA_ASREL
# GeoLite2 databases are never bundled into this image.
#
# Build for a specific platform (for example OCI Ampere A1) with buildx:
#   docker buildx build --platform linux/arm64 -t chronos:arm64 -f deploy/Dockerfile .

# ---- Frontend build stage -------------------------------------------------
FROM node:22-slim AS frontend
WORKDIR /app/frontend
COPY frontend/package.json frontend/package-lock.json* ./
RUN npm ci || npm install
COPY frontend/ ./
RUN npm run build

# ---- Rust build stage -----------------------------------------------------
FROM rust:1.96-slim AS backend
WORKDIR /app
# Cache dependency compilation by copying manifests first.
COPY Cargo.toml Cargo.lock* ./
COPY crates/ ./crates/
RUN cargo build --release --locked --bin chronos-server \
    || cargo build --release --bin chronos-server

# ---- Runtime stage --------------------------------------------------------
FROM debian:bookworm-slim AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Create a non-root user.
RUN useradd --system --uid 10001 --home /app chronos
WORKDIR /app

COPY --from=backend /app/target/release/chronos-server /usr/local/bin/chronos-server
COPY --from=frontend /app/frontend/dist /app/static

ENV CHRONOS_BIND_ADDR=0.0.0.0:8080
# Single writable directory for cached datasets and volatile runtime state.
# Mount a host path here in production to persist the CAIDA cache across
# restarts; it must be writable by the non-root runtime user.
ENV CHRONOS_DATA_DIR=/data
RUN mkdir -p /data && chown chronos:chronos /data
VOLUME ["/data"]

EXPOSE 8080
USER chronos
ENTRYPOINT ["/usr/local/bin/chronos-server"]
