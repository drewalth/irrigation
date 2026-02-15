# Contributing

## Highest-Impact Work: ESP32 Sensor Nodes

The single biggest improvement this project needs is replacing the Raspberry Pi Zero sensor nodes with ESP32-based nodes. The current Pi Zero nodes work but are expensive, power-hungry, supply-constrained, and wildly overpowered for the task (read an ADC, publish MQTT, sleep).

An ESP32 node would:

- Cost ~$5 vs ~$15+ (when you can find a Pi Zero at all)
- Run on battery with deep-sleep between readings
- Eliminate the SD card, Linux OS, and systemd service complexity
- Boot and publish in seconds, not minutes

### What needs to happen

The hub side is already node-agnostic — it consumes MQTT messages in a documented format (`tele/<node_id>/reading`) and doesn't care what sent them. The work is:

1. **ESP32 firmware** — Read the ADS1115 over I2C, connect to WiFi, publish the same JSON payload to the MQTT broker, deep-sleep until the next reading. Arduino or ESP-IDF, either works.
2. **Provisioning workflow** — How does a user configure WiFi credentials, MQTT broker address, and node ID on a device with no SSH? (Captive portal, serial config, hardcoded + reflash, etc.)
3. **Walkthrough update** — New shopping list entries, wiring diagrams, and deployment steps for ESP32 nodes alongside (or replacing) the Pi Zero instructions.

If you're interested in tackling any of this, open an issue to discuss the approach before writing code.

## General Guidelines

- Open an issue before starting significant work.
- Keep PRs focused — one logical change per PR.
- Follow the existing code style. Run `cargo fmt` and `cargo clippy` before submitting.
- If your change affects the MQTT contract, config format, or hardware wiring, update the relevant docs.
