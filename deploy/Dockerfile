# Multi-stage, multi-arch build for the Chronos server plus the static frontend.
#
# Data files (GeoLite2 .mmdb and CAIDA AS relationship datasets) are NEVER copied
# into this image. They are mounted at runtime (see the README) and their paths
# are supplied through environment variables:
#   CHRONOS_GEOLITE2_CITY_DB, CHRONOS_GEOLITE2_ASN_DB, CHRONOS_CAIDA_ASREL
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
FROM rust:1.83-slim AS backend
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
# Directory where operators are expected to mount the read only data files.
VOLUME ["/data"]

EXPOSE 8080
USER chronos
ENTRYPOINT ["/usr/local/bin/chronos-server"]
