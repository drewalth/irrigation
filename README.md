🌱 Irrigation — DIY Distributed IoT Plant Watering System

A low-cost, distributed, Rust-powered home irrigation system designed to manage large numbers of plants automatically using soil moisture telemetry, gravity-fed watering, and safe, state-driven control logic.

This project was built to solve a real problem: managing a lot of plants without turning watering into a daily manual task — while avoiding the reliability and cost limitations of commercial smart irrigation systems.

⸻

✨ Goals
	•	✅ Low cost hardware
	•	✅ Reliable and fail-safe operation (no accidental flooding)
	•	✅ Scales to dozens of plants
	•	✅ Fully local (no cloud dependency)
	•	✅ Distributed architecture
	•	✅ Learn and apply Rust in embedded + systems contexts
	•	✅ Extensible platform for experimentation

⸻

🧠 System Overview

The system uses a hub-and-node architecture:

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


⸻

🏗 Architecture

Hub (Raspberry Pi 5)

The hub is the system brain.

Responsibilities:
	•	Runs MQTT broker
	•	Receives sensor telemetry
	•	Executes irrigation control logic
	•	Drives valve relays via GPIO
	•	Enforces safety constraints
	•	Logs watering history

The hub decides when watering happens — sensors never directly control valves.

⸻

Sensor Nodes (Raspberry Pi Zero)

Distributed nodes placed near plants.

Responsibilities:
	•	Read soil moisture sensors
	•	Publish telemetry periodically
	•	Remain simple and stateless

Nodes do not make watering decisions.

⸻

Irrigation Strategy

Instead of continuous watering, the system uses:

Pulse + Soak Irrigation
	1.	Moisture drops below threshold
	2.	Valve opens briefly (“pulse”)
	3.	Water absorbs into soil (“soak” period)
	4.	Moisture re-evaluated
	5.	Repeat if necessary

This prevents:
	•	runoff
	•	sensor lag problems
	•	overwatering
	•	oscillating valve behavior

⸻

💧 Water System

Water delivery is intentionally simple:
	•	Elevated reservoir drum
	•	Gravity-fed drip irrigation
	•	Normally-closed solenoid valves
	•	Zone-based watering

Advantages:
	•	silent operation
	•	low power usage
	•	fewer failure points
	•	inexpensive hardware

⸻

🔌 Communication (MQTT)

MQTT provides lightweight, reliable messaging between devices.

Telemetry

tele/<node_id>/reading

Example payload:

{
  "ts": 1700000000,
  "readings": [
    { "sensor_id": "s1", "raw": 23110 },
    { "sensor_id": "s2", "raw": 19804 }
  ]
}

Valve Control

valve/<zone_id>/set

Payload:

ON
OFF


⸻

🦀 Why Rust?

This project intentionally uses Rust to explore:
	•	async systems programming
	•	embedded Linux development
	•	hardware interaction
	•	reliability through strong typing
	•	long-running service safety

Rust provides memory safety and predictable performance — important for a system controlling physical hardware.

⸻

🔒 Safety Design

Irrigation systems can cause real damage if they fail. Safety is a first-class concern.

Implemented protections:
	•	✅ Normally-closed valves
	•	✅ All valves OFF on startup
	•	✅ Automatic valve shutdown on errors
	•	✅ Sensor staleness detection
	•	✅ Daily watering limits
	•	✅ Time-bounded valve activation
	•	✅ Hub-controlled actuation only

Future safeguards:
	•	reservoir empty detection
	•	leak detection
	•	watchdog timers

⸻

📦 Project Structure

irrigation/
├── crates/
│   ├── hub/        # Pi 5 controller + GPIO driver
│   └── node/       # Pi Zero sensor publisher
└── Cargo.toml      # Rust workspace


⸻

🚀 Getting Started

1. Install MQTT Broker (Hub)

sudo apt install mosquitto mosquitto-clients
sudo systemctl enable --now mosquitto


⸻

2. Run Hub

export MQTT_HOST=127.0.0.1
export RELAY_ACTIVE_LOW=true

cargo run -p irrigation-hub


⸻

3. Run Sensor Node

export MQTT_HOST=<HUB_IP>
export NODE_ID=node-a
export SAMPLE_EVERY_S=30

cargo run -p irrigation-node


⸻

4. Test Valve Control

mosquitto_pub -t "valve/zone1/set" -m "ON"
mosquitto_pub -t "valve/zone1/set" -m "OFF"


⸻

🔧 Hardware (V1)

Recommended components:
	•	Raspberry Pi 5 (hub)
	•	Raspberry Pi Zero W (sensor nodes)
	•	Capacitive soil moisture sensors
	•	ADS1115 ADC (I2C)
	•	Relay board (optically isolated preferred)
	•	12V normally-closed solenoid valves
	•	Drip irrigation tubing
	•	Elevated water reservoir

⸻

🗺 Roadmap

Near Term
	•	ADS1115 sensor integration
	•	Moisture calibration workflow
	•	Zone state machine
	•	SQLite persistence
	•	Automatic watering logic

Mid Term
	•	Web dashboard
	•	Historical moisture graphs
	•	Predictive watering
	•	Remote configuration via MQTT

Future Ideas
	•	ESP32 battery-powered nodes
	•	Machine learning moisture prediction
	•	Weather integration
	•	Leak detection sensors

⸻

⚠️ Disclaimer

This project controls real water valves. Improper configuration or hardware wiring can cause flooding or property damage.

Use at your own risk and test thoroughly before unattended operation.

⸻

❤️ Philosophy

Commercial “smart plant” products often optimize for convenience over transparency.

This project prioritizes:
	•	understanding over automation
	•	reliability over novelty
	•	local control over cloud dependence

…and learning by building.
