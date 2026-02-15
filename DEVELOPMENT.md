# Development Guide

## Setup

```bash
make setup
```

This checks for required tools (node, npm, cargo, sqlite3), validates the Node.js version against `.nvmrc`, installs UI dependencies, and creates the compile-time SQLite database for sqlx macros.

If you don't have the right Node version:

```bash
nvm install    # reads .nvmrc
nvm use
```

## Local Dev (No Hardware)

Both crates run on a regular workstation — no Pi required.

The hub's `gpio` feature gates the `rppal` dependency. Without it, a mock `ValveBoard` logs valve state changes to stderr instead of toggling GPIO pins. The `gpio` feature is off by default, so `cargo build` works on any platform.

The node crate generates fake sensor readings via `rand` — no ADC or I2C needed.

You need a running MQTT broker:

```bash
# macOS
brew install mosquitto
brew services start mosquitto

# Linux
sudo apt install mosquitto mosquitto-clients
sudo systemctl enable --now mosquitto
```

Then run in separate terminals:

```bash
# Hub (mock GPIO, web UI on :8080)
MQTT_HOST=127.0.0.1 cargo run -p irrigation-hub

# Fake sensor node
MQTT_HOST=127.0.0.1 NODE_ID=node-a SAMPLE_EVERY_S=5 cargo run -p irrigation-node
```

Dashboard: http://localhost:8080

Poke valves manually:

```bash
mosquitto_pub -t "valve/zone1/set" -m "ON"
mosquitto_pub -t "valve/zone1/set" -m "OFF"
```

## Docker Dev Stack

`docker compose up --build` brings up the full system with zero local Rust toolchain:

| Service | Description |
|---------|-------------|
| `mqtt` | Eclipse Mosquitto broker (no auth, port 1883) |
| `hub` | Hub without `gpio` feature (mock valves) |
| `node-a`, `node-b` | Two fake sensor nodes publishing every 5s |

Hub web UI: http://localhost:8080

```bash
make docker-up      # build & start
make docker-down    # tear down
make docker-logs    # follow logs
```

The Dockerfile uses multi-stage builds with named targets (`hub`, `node`). The builder runs `cargo build --release` natively, producing x86_64 images only. Use `cross` for ARM binaries.

## Cross-Compilation & Deploy

Requires [`cross`](https://github.com/cross-rs/cross):

```bash
cargo install cross --locked
```

```bash
make cross-hub      # aarch64 for Pi 5
make cross-node     # armv6hf for Pi Zero W
make deploy-hub     # scp to pi5.local
make deploy-node    # scp to pizero.local
```

Override target hosts:

```bash
make deploy-hub HUB_HOST=192.168.1.50 REMOTE_USER=admin
```

## Makefile Reference

Run `make help` for the full target list. Key targets:

| Target | What it does |
|--------|-------------|
| `setup` | Prepare dev environment (tools, node, npm, sqlx db) |
| `build` | Build workspace (debug), including UI |
| `check` | Type-check without binaries |
| `test` | Run all workspace tests |
| `clippy` | Lint with `-D warnings` |
| `fmt` / `fmt-check` | Format / verify formatting |
| `ci` | `fmt-check` + `clippy` + `test` |
| `run-hub` / `run-node` | Run a single crate locally |
| `build-ui` | Build web UI (npm ci + vite build) |

## Environment Variables

| Variable | Used by | Default | Notes |
|----------|---------|---------|-------|
| `MQTT_HOST` | hub, node | `127.0.0.1` (hub), `192.168.1.10` (node) | See gotchas below |
| `MQTT_PORT` | hub, node | `1883` | |
| `RELAY_ACTIVE_LOW` | hub | `true` | `true`/`1` for active-low relay boards |
| `NODE_ID` | node | `node-a` | Must be unique per node |
| `SAMPLE_EVERY_S` | node | `300` (5 min) | Seconds between readings |
| `WEB_PORT` | hub | `8080` | Web UI listen port |
| `DB_URL` | hub | `sqlite:crates/hub/irrigation.db?mode=rwc` | Runtime database path |
| `CONFIG_PATH` | hub | `config.toml` | Zone/sensor configuration file |

### Operation Mode

The `mode` field in `config.toml` controls whether the system operates in `auto` (default) or `monitor` mode. In monitor mode, no GPIO pins are claimed and all valve actuation is blocked.

## Gotchas

1. **`gpio` feature = compile error on non-Pi.**
   `rppal` links against `/dev/gpiomem`. The feature is off by default, so this shouldn't bite you unless you enable it explicitly. Docker handles this automatically.

2. **Hub and node have different `MQTT_HOST` defaults.**
   Hub defaults to `127.0.0.1`, node defaults to `192.168.1.10`. When running locally, set `MQTT_HOST=127.0.0.1` for the node or it will silently fail to connect.

3. **Mosquitto must be running first.**
   There is no embedded broker. If MQTT is down, the hub enters a reconnect loop (2s backoff) and turns all valves off as a fail-safe.

4. **Web UI is baked into the binary.**
   `index.html` is compiled in via `include_str!`. Editing UI code requires `make build` (or `make build-ui` + `cargo build`). No live-reload. In Docker: `docker compose up --build`.

5. **Docker images are x86_64 only.**
   For ARM binaries (Pi deploy), use `cross`, not Docker.

6. **Sensor data is entirely fake.**
   The node crate generates random values. Real ADS1115 integration is on the roadmap.
