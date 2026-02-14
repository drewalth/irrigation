# ── Builder ───────────────────────────────────────────────────────
FROM rust:1-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends \
    pkg-config libsqlite3-dev sqlite3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY . .

# Create the compile-time SQLite DB that sqlx::query! macros validate against.
RUN sqlite3 crates/hub/irrigation.db < crates/hub/migrations/0001_init.sql

# Hub: build without gpio (mock valves — no rppal needed in container)
RUN cargo build --release -p irrigation-hub

# Node: standard build (already uses fake sensor data)
RUN cargo build --release -p irrigation-node

# ── Hub runtime ──────────────────────────────────────────────────
FROM debian:bookworm-slim AS hub

RUN apt-get update && apt-get install -y --no-install-recommends \
    libsqlite3-0 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/irrigation-hub /usr/local/bin/
CMD ["irrigation-hub"]

# ── Node runtime ─────────────────────────────────────────────────
FROM debian:bookworm-slim AS node

COPY --from=builder /app/target/release/irrigation-node /usr/local/bin/
CMD ["irrigation-node"]
