# Irrigation — DIY Distributed IoT Plant Watering System

![Irrigation System](https://www.drewalth.com/_astro/irrigation.02sYVLOS_tTYGC.webp)

Another DIY irrigation system... but this one is different.

A low-cost, distributed, Rust-powered home irrigation system. Soil moisture telemetry, gravity-fed watering, and safe, state-driven control logic — all running locally on Raspberry Pi hardware.

Built to manage a lot of plants without turning watering into a daily manual task, and without the reliability and cost limitations of commercial smart irrigation systems.

## Goals

- Low cost hardware
- Reliable and fail-safe operation (no accidental flooding)
- Scales to dozens of plants
- Fully local — no cloud dependency
- Distributed hub-and-node architecture
- Extensible platform for experimentation

## System Overview

```
             ┌──────────────────────┐
             │   Raspberry Pi 5     │
             │        HUB           │
             │                      │
             │  MQTT Broker         │
             │  Irrigation Control  │
             │  Valve GPIO Driver   │
             │  Web Dashboard       │
             └─────────┬────────────┘
                       MQTT
        ┌──────────────┴──────────────┐
        │                              │
┌───────────────┐            ┌───────────────┐
│ Pi Zero Node  │            │ Pi Zero Node  │
│ (Sensors)     │            │ (Sensors)     │
└───────────────┘            └───────────────┘

                 ↓
          Gravity-fed water drum
                 ↓
            Zone valves → Plants
```

### Hub (Raspberry Pi 5)

The hub is the system brain. It runs the MQTT broker, receives sensor telemetry, executes irrigation control logic, drives valve relays via GPIO, enforces safety constraints, persists data to SQLite, and serves the web dashboard.

The hub decides when watering happens — sensors never directly control valves.

### Sensor Nodes (Raspberry Pi Zero)

Lightweight nodes placed near plants. They read soil moisture sensors, publish telemetry periodically over MQTT, and remain simple and stateless. Nodes do not make watering decisions.

### Irrigation Strategy

The system uses pulse-and-soak irrigation: when moisture drops below a threshold, a valve opens briefly (pulse), water absorbs into the soil (soak period), then moisture is re-evaluated. This prevents runoff, sensor lag issues, overwatering, and oscillating valve behavior.

## Operation Modes

The system supports two operation modes, configured via `mode` in `config.toml`:

| Mode | Description |
|------|-------------|
| `auto` (default) | Full irrigation control — the scheduler monitors soil moisture and automatically opens/closes valves using pulse/soak watering cycles. |
| `monitor` | Soil moisture monitoring only — no valve actuation. The scheduler still evaluates moisture levels and records low-moisture alerts in the event log, visible on the dashboard. Ideal for deployments without valve hardware. |

In monitor mode:
- All valve commands (both scheduler-driven and manual) are blocked.
- Valve-specific config fields (`pulse_sec`, `soak_min`, `max_open_sec_per_day`, `max_pulses_per_day`, `valve_gpio_pin`) become optional with sensible defaults.
- The dashboard adapts to show moisture alerts instead of valve status.

## Project Structure

```
irrigation/
├── crates/
│   ├── hub/        # Pi 5 controller, web dashboard, GPIO driver
│   └── node/       # Pi Zero sensor publisher
├── config.toml     # Zone and sensor configuration
├── Makefile        # Build, test, deploy, and setup targets
└── Cargo.toml      # Rust workspace
```

## Quick Start

### Prerequisites

- Rust (via [rustup](https://rustup.rs))
- Node.js 22+ (via [nvm](https://github.com/nvm-sh/nvm) — see `.nvmrc`)
- sqlite3
- Mosquitto MQTT broker

### Setup

```bash
make setup    # checks tools, installs UI deps, creates sqlx compile-time DB
```

### Run locally (no hardware needed)

```bash
# Start Mosquitto (macOS: brew services start mosquitto)

# Terminal 1 — hub (mock GPIO, web UI on :8080)
MQTT_HOST=127.0.0.1 cargo run -p irrigation-hub

# Terminal 2 — fake sensor node
MQTT_HOST=127.0.0.1 NODE_ID=node-a SAMPLE_EVERY_S=5 cargo run -p irrigation-node
```

Dashboard: http://localhost:8080

### Or use Docker

```bash
make docker-up    # mqtt + hub + 2 fake nodes, UI on :8080
```

See [DEVELOPMENT.md](DEVELOPMENT.md) for the full development guide, cross-compilation, deployment, environment variables, and gotchas.

## MQTT Topics

| Topic | Direction | Payload |
|-------|-----------|---------|
| `tele/<node_id>/reading` | Node -> Hub | `{ "ts": 1700000000, "readings": [{ "sensor_id": "s1", "raw": 23110 }] }` |
| `valve/<zone_id>/set` | Hub -> Valve | `ON` / `OFF` |

## Safety

Irrigation systems can cause real damage. Safety is a first-class concern.

- Normally-closed valves (fail safe on power loss)
- All valves OFF on startup
- Automatic valve shutdown on errors
- Sensor staleness detection
- Daily watering limits (pulse count + open-seconds caps)
- Time-bounded valve activation
- Hub-controlled actuation only — sensors never drive valves

## Hardware (V1)

- Raspberry Pi 5 (hub)
- Raspberry Pi Zero W (sensor nodes)
- Capacitive soil moisture sensors
- ADS1115 ADC (I2C)
- Relay board (optically isolated preferred)
- 12V normally-closed solenoid valves
- Drip irrigation tubing
- Elevated water reservoir

## Roadmap

### Next

- ADS1115 real sensor integration
- Moisture calibration workflow
- Automatic watering logic (zone state machine triggers)
- Predictive watering
- Remote configuration via MQTT

### Future

- ESP32 battery-powered nodes
- Weather integration
- Leak / reservoir-empty detection

## Disclaimer

This project controls real water valves. Improper configuration or hardware wiring can cause flooding or property damage. Use at your own risk and test thoroughly before unattended operation.
