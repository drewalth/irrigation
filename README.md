# 🌱 Irrigation — DIY Distributed IoT Plant Watering System

A low-cost, distributed, Rust-powered home irrigation system designed to manage large numbers of plants automatically using soil moisture telemetry, gravity-fed watering, and safe, state-driven control logic.

This project was built to solve a real problem: managing a lot of plants without turning watering into a daily manual task — while avoiding the reliability and cost limitations of commercial smart irrigation systems.

## ✨ Goals

- ✅ Low cost hardware
- ✅ Reliable and fail-safe operation (no accidental flooding)
- ✅ Scales to dozens of plants
- ✅ Fully local (no cloud dependency)
- ✅ Distributed architecture
- ✅ Learn and apply Rust in embedded + systems contexts
- ✅ Extensible platform for experimentation

## 🧠 System Overview

The system uses a hub-and-node architecture:

```
             ┌──────────────────────┐
             │   Raspberry Pi 5     │
             │        HUB           │
             │                      │
             │  MQTT Broker         │
             │  Irrigation Control  │
             │  Valve GPIO Driver   │
             └─────────┬────────────┘
                       MQTT
        ┌──────────────┴──────────────┐
        │                              │
┌───────────────┐            ┌───────────────┐
│ Pi Zero Node  │            │ Pi Zero Node  │
│ (Sensors)     │            │ (Sensors)     │
│               │            │               │
│ Soil Sensors  │            │ Soil Sensors  │
└───────────────┘            └───────────────┘

                 ↓
          Gravity-fed water drum
                 ↓
            Zone valves
                 ↓
               Plants 🌿
```

## 🏗 Architecture

### Hub (Raspberry Pi 5)

The hub is the system brain.

Responsibilities:
- Runs MQTT broker
- Receives sensor telemetry
- Executes irrigation control logic
- Drives valve relays via GPIO
- Enforces safety constraints
- Logs watering history

The hub decides when watering happens — sensors never directly control valves.

### Sensor Nodes (Raspberry Pi Zero)

Distributed nodes placed near plants.

Responsibilities:
- Read soil moisture sensors
- Publish telemetry periodically
- Remain simple and stateless

Nodes do not make watering decisions.

### Irrigation Strategy

Instead of continuous watering, the system uses:

**Pulse + Soak Irrigation**
1. Moisture drops below threshold
2. Valve opens briefly ("pulse")
3. Water absorbs into soil ("soak" period)
4. Moisture re-evaluated
5. Repeat if necessary

This prevents:
- runoff
- sensor lag problems
- overwatering
- oscillating valve behavior

## 💧 Water System

Water delivery is intentionally simple:
- Elevated reservoir drum
- Gravity-fed drip irrigation
- Normally-closed solenoid valves
- Zone-based watering

Advantages:
- silent operation
- low power usage
- fewer failure points
- inexpensive hardware

## 🔌 Communication (MQTT)

MQTT provides lightweight, reliable messaging between devices.

### Telemetry

`tele/<node_id>/reading`

Example payload:

```json
{
  "ts": 1700000000,
  "readings": [
    { "sensor_id": "s1", "raw": 23110 },
    { "sensor_id": "s2", "raw": 19804 }
  ]
}
```

### Valve Control

`valve/<zone_id>/set`

Payload:

```
ON
OFF
```

## 🦀 Why Rust?

This project intentionally uses Rust to explore:
- async systems programming
- embedded Linux development
- hardware interaction
- reliability through strong typing
- long-running service safety

Rust provides memory safety and predictable performance — important for a system controlling physical hardware.

## 🔒 Safety Design

Irrigation systems can cause real damage if they fail. Safety is a first-class concern.

Implemented protections:
- ✅ Normally-closed valves
- ✅ All valves OFF on startup
- ✅ Automatic valve shutdown on errors
- ✅ Sensor staleness detection
- ✅ Daily watering limits
- ✅ Time-bounded valve activation
- ✅ Hub-controlled actuation only

Future safeguards:
- reservoir empty detection
- leak detection
- watchdog timers

## 📦 Project Structure

```
irrigation/
├── crates/
│   ├── hub/        # Pi 5 controller + GPIO driver
│   └── node/       # Pi Zero sensor publisher
└── Cargo.toml      # Rust workspace
```

## 🚀 Getting Started

### 1. Install MQTT Broker (Hub)

```bash
sudo apt install mosquitto mosquitto-clients
sudo systemctl enable --now mosquitto
```

### 2. Run Hub

```bash
export MQTT_HOST=127.0.0.1
export RELAY_ACTIVE_LOW=true

cargo run -p irrigation-hub
```

### 3. Run Sensor Node

```bash
export MQTT_HOST=<HUB_IP>
export NODE_ID=node-a
export SAMPLE_EVERY_S=30

cargo run -p irrigation-node
```

### 4. Test Valve Control

```bash
mosquitto_pub -t "valve/zone1/set" -m "ON"
mosquitto_pub -t "valve/zone1/set" -m "OFF"
```

## 🛠 Development

### Local Dev (No Hardware)

Both crates are designed to run on a regular workstation — no Pi required.

The hub uses a Cargo feature flag `gpio` (on by default) to gate the `rppal` dependency. To build without hardware:

```bash
cargo build -p irrigation-hub --no-default-features
```

This swaps in a mock `ValveBoard` that logs valve state changes to stderr instead of toggling GPIO pins.

The node crate generates fake sensor readings via `rand` out of the box — no ADC or I2C needed.

You still need a running MQTT broker. Install Mosquitto locally:

```bash
# macOS
brew install mosquitto
brew services start mosquitto

# Linux
sudo apt install mosquitto mosquitto-clients
sudo systemctl enable --now mosquitto
```

Then run the hub and one or more nodes in separate terminals:

```bash
# Terminal 1 — hub (mock GPIO, web UI on :8080)
MQTT_HOST=127.0.0.1 cargo run -p irrigation-hub --no-default-features

# Terminal 2 — fake node
MQTT_HOST=127.0.0.1 NODE_ID=node-a SAMPLE_EVERY_S=5 cargo run -p irrigation-node
```

The web dashboard is available at `http://localhost:8080`. It is embedded into the binary via `include_str!`, so changes to `crates/hub/src/ui/index.html` require a recompile.

You can also poke valves manually with mosquitto_pub:

```bash
mosquitto_pub -t "valve/zone1/set" -m "ON"
mosquitto_pub -t "valve/zone1/set" -m "OFF"
```

### Docker Dev Stack

`docker compose up --build` brings up the full system with zero local Rust toolchain setup:

| Service | Description |
|---------|-------------|
| `mqtt` | Eclipse Mosquitto broker (no auth, port 1883) |
| `hub` | Hub built **without** the `gpio` feature (mock valves) |
| `node-a`, `node-b` | Two fake sensor nodes publishing every 5s |

The hub web UI is exposed at `http://localhost:8080`.

```bash
# Start everything (foreground, with build)
make docker-up          # or: docker compose up --build

# Tear down
make docker-down

# Tail logs
make docker-logs
```

The Dockerfile uses a multi-stage build with named targets (`hub`, `node`) — the compose file references these via the `target` field. The builder stage runs `cargo build --release` natively, so this only produces x86_64 images (not ARM). Use `cross` for Pi-targeted binaries.

### Cross-Compilation & Deploy

Cross-compilation requires [`cross`](https://github.com/cross-rs/cross):

```bash
cargo install cross --locked
```

Then:

```bash
make cross-hub          # aarch64 for Pi 5
make cross-node         # armv6hf for Pi Zero W
make deploy-hub         # scp to pi5.local
make deploy-node        # scp to pizero.local
```

Override target hosts:

```bash
make deploy-hub HUB_HOST=192.168.1.50 REMOTE_USER=admin
```

### Makefile Reference

| Target | What it does |
|--------|-------------|
| `build` | `cargo build --workspace` (debug) |
| `check` | Type-check without binaries |
| `test` | Run all workspace tests |
| `clippy` | Lint with `-D warnings` |
| `fmt` / `fmt-check` | Format / verify formatting |
| `ci` | `fmt-check` + `clippy` + `test` |
| `run-hub` / `run-node` | Run a single crate locally |

### Environment Variables

| Variable | Used by | Default | Notes |
|----------|---------|---------|-------|
| `MQTT_HOST` | hub, node | `127.0.0.1` (hub), `192.168.1.10` (node) | See gotchas |
| `MQTT_PORT` | hub, node | `1883` | |
| `RELAY_ACTIVE_LOW` | hub | `true` | `true`/`1` for active-low relay boards |
| `NODE_ID` | node | `node-a` | Must be unique per node |
| `SAMPLE_EVERY_S` | node | `300` (5 min) | Seconds between readings |
| `WEB_PORT` | hub | `8080` | Web UI listen port |

### Gotchas

1. **`gpio` feature = compile error on non-Pi.**
   The default feature set includes `rppal`, which links against `/dev/gpiomem`. If you forget `--no-default-features` on a Mac/x86 Linux box, the build will fail. Docker handles this automatically.

2. **Hub and node have different `MQTT_HOST` defaults.**
   Hub defaults to `127.0.0.1`, node defaults to `192.168.1.10`. When running locally, set `MQTT_HOST=127.0.0.1` explicitly for the node or it will silently fail to connect.

3. **Mosquitto must be running first.**
   There is no embedded broker. If MQTT is down the hub enters a reconnect loop (2s backoff) and turns all valves off as a fail-safe. Nodes log errors and retry independently.

4. **Web UI is baked into the binary.**
   `index.html` is compiled in via `include_str!`. Editing it requires a `cargo build` — there is no live-reload. In Docker you need a full `docker compose up --build`.

5. **Docker images are x86_64 only.**
   The Dockerfile builds natively inside the Rust container. For ARM binaries (Pi deploy), use `cross`, not Docker.

6. **Sensor data is entirely fake.**
   The node crate currently generates random values between 17000–26000. Real ADS1115 integration is on the roadmap.

## 🔧 Hardware (V1)

Recommended components:
- Raspberry Pi 5 (hub)
- Raspberry Pi Zero W (sensor nodes)
- Capacitive soil moisture sensors
- ADS1115 ADC (I2C)
- Relay board (optically isolated preferred)
- 12V normally-closed solenoid valves
- Drip irrigation tubing
- Elevated water reservoir

## 🗺 Roadmap

### Near Term

- ADS1115 sensor integration
- Moisture calibration workflow
- Zone state machine
- SQLite persistence
- Automatic watering logic

### Mid Term

- Web dashboard with commands + configuration
- Historical moisture graphs
- Predictive watering
- Remote configuration via MQTT

### Future Ideas

- ESP32 battery-powered nodes
- Machine learning moisture prediction
- Weather integration
- Leak detection sensors

## ⚠️ Disclaimer

This project controls real water valves. Improper configuration or hardware wiring can cause flooding or property damage.

Use at your own risk and test thoroughly before unattended operation.

## ❤️ Philosophy

Commercial "smart plant" products often optimize for convenience over transparency.

This project prioritizes:
- understanding over automation
- reliability over novelty
- local control over cloud dependence

…and learning by building.
