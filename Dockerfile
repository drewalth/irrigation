# ── Builder ───────────────────────────────────────────────────────
FROM rust:1-slim-bookworm AS builder

WORKDIR /app
COPY . .

# Hub: build WITHOUT the gpio feature (mock valves, no rppal)
RUN cargo build --release -p irrigation-hub --no-default-features

# Node: standard build (already uses fake sensor data)
RUN cargo build --release -p irrigation-node

# ── Hub runtime ──────────────────────────────────────────────────
FROM debian:bookworm-slim AS hub

COPY --from=builder /app/target/release/irrigation-hub /usr/local/bin/
CMD ["irrigation-hub"]

# ── Node runtime ─────────────────────────────────────────────────
FROM debian:bookworm-slim AS node

COPY --from=builder /app/target/release/irrigation-node /usr/local/bin/
CMD ["irrigation-node"]
