# Walkthrough: From Zero to Automated Irrigation

This guide walks you through building a complete DIY irrigation system from scratch. You'll set up a Raspberry Pi 5 as a central hub, wire two Pi Zero 2 W sensor nodes, and go from bare hardware to a system that waters your plants automatically based on real soil moisture data.

**Who this is for:** You're comfortable with Linux, terminals, Rust, Docker, and maybe getting electrified. You haven't done much (or any) hardware work. You know what GPIO stands for in theory but have never wired anything to one. Or you just want to
dig that old Raspberry Pi out of the attic and do something fun!

**What you'll build:**

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
                       │ MQTT over WiFi
        ┌──────────────┴──────────────┐
        │                              │
┌───────────────┐            ┌───────────────┐
│ Pi Zero 2 W   │            │ Pi Zero 2 W   │
│ Node A        │            │ Node B        │
│ (2 sensors)   │            │ (2 sensors)   │
└───────────────┘            └───────────────┘

                 ↓
          Gravity-fed water source
                 ↓
            Solenoid valves → Plants
```

- **Hub** (Pi 5): Runs the MQTT broker, receives sensor data, makes watering decisions, drives relay-controlled solenoid valves, serves a web dashboard.
- **Nodes** (Pi Zero 2 W): Read capacitive soil moisture sensors via an ADS1115 ADC over I2C, publish readings to the hub over MQTT. Stateless — they never control valves.
- **Water source**: Gravity-fed (rain barrel, elevated tank). The valves in this guide are rated for zero-pressure operation.

Two irrigation zones, two sensor nodes, four soil moisture sensors total. See the [README](README.md) for the full architectural deep-dive.

---

## Part 1: See It Run Locally with Docker

Before buying any hardware, run the entire system on your laptop. This gives you a working mental model and confirms the software stack is healthy.

### Prerequisites

- [Docker Desktop](https://www.docker.com/products/docker-desktop/) installed and running
- Git

### Clone and start

```bash
git clone https://github.com/drewalth/irrigation.git
cd irrigation
docker compose up --build
```

This spins up four containers:

| Container | Role                                               |
| --------- | -------------------------------------------------- |
| `mqtt`    | Mosquitto MQTT broker (anonymous access, dev only) |
| `hub`     | Irrigation hub with mock GPIO (no real valves)     |
| `node-a`  | Simulated sensor node for the "front-lawn" zone    |
| `node-b`  | Simulated sensor node for the "back-garden" zone   |

The simulated nodes publish fake-but-realistic sensor data every 5 seconds (vs. every 5 minutes in production). They use the `drying` scenario by default — moisture readings will gradually decrease over time, eventually triggering the scheduler to open a valve.

### Explore the dashboard

Open [http://localhost:8080](http://localhost:8080) in your browser.

You should see:

- Two zones (Front Lawn, Back Garden) with live moisture readings
- Sensor status indicators (online/offline)
- The scheduler state for each zone (Idle, Watering, Soaking)

Watch for a few minutes. As the simulated soil dries out, the hub will trigger a watering pulse — you'll see the valve state change and the scheduler cycle through Watering → Soaking → re-evaluation.

### Poke a valve manually

In a separate terminal, publish an MQTT message to manually open a valve:

```bash
docker compose exec mqtt mosquitto_pub -t 'valve/front-lawn/set' -m 'ON'
```

Check the hub logs to see it react:

```bash
docker compose logs hub --tail 20
```

Turn it off:

```bash
docker compose exec mqtt mosquitto_pub -t 'valve/front-lawn/set' -m 'OFF'
```

### Tear down

```bash
docker compose down -v
```

You now have a solid mental model of how the pieces talk to each other. Time to build the real thing.

---

## Part 2: Shopping List

Everything you need for one hub and two sensor nodes. Prices are approximate and will vary by region.

> **A note on cost and node hardware.** This system currently uses Raspberry Pi Zero 2 W boards as sensor nodes. They work, but they're overkill for the job — a full Linux SBC to read an ADC and publish MQTT is like hiring a forklift to move a shoebox. Pi Zeros also have chronic supply problems and often sell well above their $15 MSRP. An ESP32 (~$5, built-in WiFi, deep-sleep capable, no SD card needed) is a far better fit for a battery-friendly sensor node. I just happen to have these laying around and wanted to get them working... ESP32 node support is on the [roadmap](README.md#roadmap). If you want to help make that happen sooner, see [CONTRIBUTING.md](CONTRIBUTING.md).

### Computing

| Item                                 | Qty | Est. Price | Notes                                                                                                                                                                                                                                                   |
| ------------------------------------ | --- | ---------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Raspberry Pi 5 (4 GB)                | 1   | ~$60       | Hub. 8 GB works but is overkill for this workload.                                                                                                                                                                                                      |
| Raspberry Pi Zero 2 W                | 2   | ~$15 each  | Sensor nodes. The Zero 2 W is 5x faster than the original Zero W (quad-core vs. single-core) at the same price point. Strongly recommended over the original.                                                                                           |
| microSD card, 32 GB, endurance-rated | 3   | ~$10 each  | One per Pi. Get endurance cards (Samsung PRO Endurance, SanDisk MAX Endurance). The hub writes sensor data periodically — regular cards wear out faster. The software mitigates this with tmpfs + backup, but endurance cards are still the right call. |

### Power Supplies

| Item                                           | Qty | Notes                                                                                                                                                              |
| ---------------------------------------------- | --- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Official Raspberry Pi 5 USB-C PSU (27W, 5V/5A) | 1   | The Pi 5 is power-hungry. Third-party supplies that can't sustain 5A will trigger undervoltage throttling. Use the official one.                                   |
| 5V/2.5A micro-USB PSU                          | 2   | One per Pi Zero. A decent phone charger works if it outputs stable 5V under load.                                                                                  |
| 12V/2A DC power supply (barrel jack)           | 1   | Powers both solenoid valves. Two valves at ~500 mA each = 1A steady-state; 2A gives headroom. Must be a regulated supply, not a wall wart with unregulated output. |

### Sensors and ADC

| Item                              | Qty          | Notes                                                                                                                                                                                                                                                                                                                                                     |
| --------------------------------- | ------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| ADS1115 16-bit ADC breakout board | 2            | One per node. Adafruit #1085 or any equivalent with I2C pull-ups and decoupling cap included. This is a 4-channel, 16-bit analog-to-digital converter. The software reads it over I2C.                                                                                                                                                                    |
| Capacitive soil moisture sensor   | 4 + 2 spares | Two per node (mapped to ADS1115 channels 0 and 1). **Recommended:** DFRobot SEN0193 Gravity Analog. These are corrosion-resistant and output ~1.2 V (wet) to ~2.8 V (dry) at 3.3 V, which fits the ADC's ±4.096 V range. Avoid the cheap blue "v1.2" boards — they corrode within weeks underground. Buy extras for calibration testing and replacements. |

### Relay and Valves

| Item                                                            | Qty | Notes                                                                                                                                                                                                                 |
| --------------------------------------------------------------- | --- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 2-channel relay module, 5V coil, optically isolated, active-low | 1   | The software defaults to active-low relay logic (`RELAY_ACTIVE_LOW=true`). Look for boards with optocouplers (like the EL817) between the Pi and the relay coils. A 4-channel board is fine if you want room to grow. |
| 12V DC solenoid valve, normally-closed, zero-differential       | 2   | Read both of those adjectives carefully — they matter.                                                                                                                                                                |
| 1N4007 flyback diode                                            | 2   | One per valve. Protects relay contacts from voltage spikes when the valve coil de-energizes.                                                                                                                          |

**On valve selection — this is important:**

_Normally-closed_ means the valve is shut when no power is applied. If the Pi crashes, the power goes out, or the software panics, the valve springs closed and water stops. This is the system's primary safety mechanism. It is not optional.

_Zero-differential_ (also called "direct-acting") means the valve can open with zero water pressure behind it. Standard solenoid valves need 0.3–0.5 bar minimum inlet pressure to function. Since you're using a gravity-fed water source, there's almost no pressure. A standard valve will simply refuse to open. Zero-differential valves use a direct plunger mechanism instead of a pilot diaphragm, and work from 0 bar up. They cost more (~$25–40 vs ~$8–15) but are the only option for gravity-fed systems.

Look for: 12V DC, normally-closed, 0 bar minimum operating pressure, 1/2" BSP thread, brass or stainless body.

### Wiring and Connectors

| Item                                          | Notes                                                                                                                           |
| --------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| Dupont jumper wires, female-to-female, 20 cm  | For connecting Pi GPIO headers to the relay board and ADS1115 breakouts. Get a pack of 40.                                      |
| 22 AWG stranded wire, silicone-jacketed       | For sensor wiring (3.3V, GND, signal). Silicone jacket is UV-resistant and stays flexible in cold weather. 5–10 meters.         |
| 18 AWG stranded wire, 2-conductor             | For the 12V valve circuit. Heavier gauge for the higher-current path. 3–5 meters.                                               |
| Screw terminal blocks, 2-position, 5 mm pitch | For clean valve and power connections. 4–6 blocks.                                                                              |
| Wire ferrule kit + crimp tool                 | Prevents stranded wire from fraying in screw terminals. A ~$15 investment that prevents intermittent connections down the road. |
| Adhesive-lined heat-shrink tubing, assorted   | For waterproofing outdoor wire splices. The adhesive lining melts and seals when heated.                                        |

### Enclosures (for outdoor deployment)

| Item                                             | Qty   | Notes                                                                                    |
| ------------------------------------------------ | ----- | ---------------------------------------------------------------------------------------- |
| IP65 polycarbonate junction box, ~200x150x100 mm | 1     | Hub Pi + relay board. Polycarbonate is UV-resistant (ABS yellows and cracks).            |
| IP65 polycarbonate junction box, ~150x100x70 mm  | 2     | One per sensor node (Pi Zero + ADS1115).                                                 |
| PG7 cable glands, nylon, IP68                    | 10    | For every wire penetrating an enclosure. Each box needs 2–4 glands.                      |
| M2.5 nylon standoffs, 11 mm                      | 1 set | For mounting Pi boards inside enclosures. Pi Zero and Pi 5 both use M2.5 mounting holes. |

### Prototyping

| Item                            | Notes                                                                                                                                        |
| ------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------- |
| Half-size solderless breadboard | For bench-testing the sensor + ADC circuit before permanent installation. Do not use a breadboard outdoors — connections corrode and loosen. |

---

## Part 3: Preparing the Raspberry Pis

### Flash the SD cards

Download and install [Raspberry Pi Imager](https://www.raspberrypi.com/software/).

You'll flash three cards — one for the hub and one for each node. The Pi 5 and Pi Zero 2 W require different OS images.

**Hub (Pi 5):**

1. Open Raspberry Pi Imager.
2. Choose device: **Raspberry Pi 5**.
3. Choose OS: **Raspberry Pi OS (64-bit)** — the full desktop image is fine, but Lite works too if you prefer headless.
4. Click the gear icon (or "Edit Settings") to configure:
   - **Hostname:** `pi5`
   - **Enable SSH:** Yes, use password authentication
   - **Username:** `pi` (with a password you'll remember)
   - **Configure WiFi:** Enter your SSID and password (or skip if you'll use Ethernet)
5. Flash to the first SD card.

**Node A (Pi Zero 2 W):**

1. Choose device: **Raspberry Pi Zero 2 W**.
2. Choose OS: **Raspberry Pi OS Lite (32-bit)** — no desktop needed, these are headless.
3. Configure:
   - **Hostname:** `pizero-a`
   - **Enable SSH:** Yes
   - **Username:** `pi`
   - **WiFi:** Same network as the hub
4. Flash to the second SD card.

**Node B:** Repeat the above with hostname `pizero-b`.

### Boot and verify SSH

Insert the SD cards, plug in power, and wait about 60–90 seconds for the first boot. Then verify you can reach each Pi:

```bash
ssh pi@pi5.local
ssh pi@pizero-a.local
ssh pi@pizero-b.local
```

If `.local` hostnames don't resolve (some routers don't support mDNS well), find the IPs from your router's DHCP client list and use those directly. Consider assigning static IPs or DHCP reservations — you'll want stable addresses.

### Hub: Install Mosquitto

SSH into the hub and install the MQTT broker:

```bash
ssh pi@pi5.local
sudo apt update && sudo apt install -y mosquitto mosquitto-clients
```

Set up password authentication. You'll create two MQTT users — one for the hub, one shared by the nodes:

```bash
sudo mosquitto_passwd -c /etc/mosquitto/passwd irrigation-hub
# Enter a password when prompted

sudo mosquitto_passwd /etc/mosquitto/passwd irrigation-node
# Enter a different password when prompted
```

The production Mosquitto config is in the repo at `deploy/mosquitto-production.conf`. You'll copy this to the Pi in the deployment step later. For now, Mosquitto is installed and will start with default settings.

Create an ACL file to restrict topic access:

```bash
sudo tee /etc/mosquitto/acl << 'EOF'
user irrigation-hub
topic readwrite #

user irrigation-node
topic write tele/+/reading
topic read valve/+/set
EOF
```

You'll activate password auth and the ACL during deployment. For now, verify Mosquitto is running:

```bash
sudo systemctl status mosquitto
```

### Nodes: Enable I2C

SSH into each Pi Zero and enable the I2C bus. The ADS1115 ADC communicates over I2C.

```bash
ssh pi@pizero-a.local
sudo raspi-config
```

Navigate to: **Interface Options** → **I2C** → **Enable** → **Finish** → **Reboot**.

After reboot, verify I2C is available:

```bash
ls /dev/i2c-1
```

If the file exists, I2C is enabled. Also add the `pi` user to the `i2c` group so the node binary can access the bus without root:

```bash
sudo usermod -aG i2c pi
```

Log out and back in (or reboot) for the group change to take effect.

Repeat for `pizero-b.local`.

### Hub: Verify GPIO access

On the Pi 5, the `pi` user should already be in the `gpio` group. Verify:

```bash
groups pi
```

If `gpio` is not listed:

```bash
sudo usermod -aG gpio pi
```

---

## Part 4: Wiring the Hardware

This is the section where software engineers tend to get nervous. That's fine. The circuits here are simple and low-voltage. Take your time, double-check connections before applying power, and you won't get zapped.

A few ground rules before you start:

- **Never connect 5V to a Pi GPIO pin.** The Pi's processor is 3.3V logic and is not 5V tolerant. Applying 5V to any GPIO pin will permanently destroy the chip.
- **Never connect a valve (or any motor/solenoid) directly to a GPIO pin.** GPIO pins can source about 16 mA at 3.3V. A solenoid valve draws 300–800 mA at 12V. Always go through the relay.
- **The 12V valve circuit is completely separate from the 5V Pi circuit.** They share a common ground through the relay board, but the 12V supply never touches the Pi.

### A quick note on GPIO numbering

The Pi has a 40-pin header. There are two ways to refer to pins:

- **Physical pin numbers:** Counting from pin 1 (top-left when the Pi's USB ports face you). 1–40.
- **BCM (Broadcom) GPIO numbers:** The chip's internal signal names. GPIO 17 is physical pin 11, not pin 17.

**This software uses BCM numbering.** The `rppal` Rust crate uses BCM, `config.toml` uses BCM, and all references in this guide use BCM. When you're wiring, you need to translate BCM to physical pins. Here's the subset you'll use:

| BCM GPIO | Physical Pin | Function in this system             |
| -------- | ------------ | ----------------------------------- |
| 2        | Pin 3        | I2C SDA (node → ADS1115)            |
| 3        | Pin 5        | I2C SCL (node → ADS1115)            |
| 17       | Pin 11       | Relay IN1 — front-lawn valve (hub)  |
| 27       | Pin 13       | Relay IN2 — back-garden valve (hub) |

For a complete pinout, run `pinout` on any Pi or visit [pinout.xyz](https://pinout.xyz). Consider printing a pinout diagram and taping it to your work surface.

### 4A: Sensor nodes — ADS1115 + soil moisture sensors

Each node gets identical wiring: a Pi Zero 2 W connected to one ADS1115 breakout board and two soil moisture sensors.

**Pi Zero 2 W → ADS1115:**

| Pi Zero Pin | Pi Zero Function | ADS1115 Pin |
| ----------- | ---------------- | ----------- |
| Pin 1       | 3.3V             | VDD         |
| Pin 3       | GPIO 2 (SDA)     | SDA         |
| Pin 5       | GPIO 3 (SCL)     | SCL         |
| Pin 9       | GND              | GND         |
| —           | —                | ADDR → GND  |

Connect the ADS1115's ADDR pin to GND. This sets its I2C address to `0x48`, which is the software default. If you leave ADDR floating, the address is unpredictable and the driver won't find the chip.

**Soil moisture sensors → ADS1115:**

Each sensor has three wires: VCC, GND, and AOUT (analog output).

| Sensor Wire     | Connect To           |
| --------------- | -------------------- |
| VCC             | Pi Zero Pin 1 (3.3V) |
| GND             | Pi Zero Pin 9 (GND)  |
| AOUT (Sensor 1) | ADS1115 AIN0         |
| AOUT (Sensor 2) | ADS1115 AIN1         |

Power the sensors from 3.3V, not 5V. At 3.3V the sensor output ranges from ~1.2V (wet) to ~2.8V (dry), which is well within the ADS1115's ±4.096V measurement range. At 5V, the output range shifts higher and can exceed the ADC's input limit.

```
Pi Zero 2 W                   ADS1115 Breakout
─────────────                  ─────────────────
Pin 1  (3.3V)  ──────────────▶ VDD
Pin 3  (GPIO 2 / SDA) ───────▶ SDA
Pin 5  (GPIO 3 / SCL) ───────▶ SCL
Pin 9  (GND)   ──────────────▶ GND
                                ADDR ──▶ GND

                                AIN0 ◀── Sensor 1 signal (AOUT)
                                AIN1 ◀── Sensor 2 signal (AOUT)

Sensor 1 VCC ────────────────▶ Pin 1 (3.3V)
Sensor 1 GND ────────────────▶ Pin 9 (GND)

Sensor 2 VCC ────────────────▶ Pin 1 (3.3V)
Sensor 2 GND ────────────────▶ Pin 9 (GND)
```

For bench testing, use the breadboard and Dupont jumper wires. For a permanent outdoor install, solder connections or use JST connectors, and run sensor cables in shielded wire if the runs exceed 1 meter.

Wire both nodes identically. The software distinguishes them by `NODE_ID` (set in the systemd service), not by any hardware difference.

### 4B: Hub — relay module

The relay module connects to the Pi 5's GPIO header.

**Pi 5 → Relay Board:**

| Pi 5 Pin | Pi 5 Function | Relay Board Pin |
| -------- | ------------- | --------------- |
| Pin 2    | 5V            | VCC             |
| Pin 6    | GND           | GND             |
| Pin 11   | GPIO 17 (BCM) | IN1             |
| Pin 13   | GPIO 27 (BCM) | IN2             |

The relay board's VCC gets 5V because the relay coils need 5V to energize. The GPIO pins output 3.3V to drive the optocoupler LEDs on the input side — this works because the optocoupler's forward voltage is ~1.2V and the onboard resistor is sized for 3.3V–5V input.

**Understanding active-low logic:**

The software initializes relay GPIO pins HIGH on startup (this is the safe/OFF state for active-low boards). Here's how it works:

| GPIO State  | Relay State        | Valve State        |
| ----------- | ------------------ | ------------------ |
| HIGH (3.3V) | De-energized (OFF) | Closed (no water)  |
| LOW (0V)    | Energized (ON)     | Open (water flows) |

This means a powered-down or crashed Pi holds all GPIOs in a safe state — relays off, valves closed, water stopped.

**How to verify your relay board is active-low:** With the board powered (VCC to 5V, GND to GND), briefly touch IN1 to GND with a jumper wire. You should hear the relay click. If it clicks when you touch IN1 to 5V instead, you have an active-high board — set `RELAY_ACTIVE_LOW=false` in the hub service file later. Active-low boards are far more common.

### 4C: Solenoid valves to relay — the 12V circuit

Each valve connects to the relay board through a simple circuit with a flyback protection diode.

**For each valve:**

```
12V DC Power Supply
─────────────────────
  (+) ──────────────────▶ Relay COM (Common)

                          Relay NO (Normally Open) ───┐
                                                      │
                          1N4007 diode:               │
                          cathode (stripe) ───────────┤
                                                      ▼
                                                  Valve (+)
                                                      │
                          Valve (-)                    │
                              │                        │
                          1N4007 diode:               │
                          anode ──────────────────────┘
                              │
  (-) ◀─────────────────────┘
```

Step by step:

1. **12V (+)** goes to the relay's **COM** (common) terminal.
2. **Relay NO** (normally open) terminal goes to the valve's positive (+) terminal.
3. The valve's negative (-) terminal goes back to **12V (-)** ground.
4. Install a **1N4007 diode across the valve**: cathode (the end with the stripe) to valve (+), anode to valve (-).

**Why the NO (normally open) relay terminal?** When the relay is de-energized (power off, Pi crashed, GPIO HIGH), the NO contact is open — no 12V reaches the valve — the normally-closed valve stays shut. Water stops. When the software drives the GPIO LOW, the relay energizes, NO closes, 12V flows, the valve opens.

**Why the flyback diode?** The solenoid valve coil is an inductor. When current is suddenly cut (relay opens), the coil generates a voltage spike that can arc across the relay contacts and degrade them over time. The diode clamps this spike. If you install the diode backwards (cathode and anode swapped), it creates a dead short across the 12V supply when the relay closes. Check the stripe orientation with a multimeter in diode mode before powering on.

Wire both valves identically — one to each relay channel.

### 4D: Pre-flight checklist

Before applying power to anything, walk through this list:

- [ ] ADS1115 ADDR pin is connected to GND (address 0x48) on both nodes
- [ ] All sensor VCC wires are on 3.3V, not 5V
- [ ] Relay board VCC is on 5V (Pin 2), not a GPIO pin
- [ ] GPIO 17 (Pin 11) goes to relay IN1, GPIO 27 (Pin 13) goes to relay IN2
- [ ] 5V goes only to Pi power inputs and relay VCC — never to a GPIO pin
- [ ] 12V goes only to relay COM terminals and valve circuit — never near the Pi
- [ ] Flyback diodes are correctly oriented: cathode (stripe) to valve (+), anode to valve (-)
- [ ] No bare wire-to-wire connections — use screw terminals or solder joints

**Power-on test sequence:**

1. Power on the Pi Zeros first (no valves connected yet). SSH in and run `i2cdetect -y 1`. You should see address `0x48` in the grid — that's the ADS1115. If you see nothing, check your SDA/SCL wiring.
2. Power on the Pi 5 with the relay board connected but no 12V supply yet. Verify SSH access.
3. Connect the 12V supply. The relay should not click (GPIO is floating/HIGH on boot).
4. After software deployment, test the fail-safe: open a valve via the dashboard, then pull the Pi's power cable. The valve must click shut within about one second. If it doesn't, something is wired wrong — stop and diagnose before deploying outdoors.

---

## Part 5: Building and Deploying the Software

### Dev machine setup

You need these tools on your development machine (macOS or Linux):

**Rust:**

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

**Docker Desktop:** Install from [docker.com](https://www.docker.com/products/docker-desktop/). Required for `cross` (the cross-compilation tool).

**cross** (Docker-based cross-compiler for ARM targets):

```bash
cargo install cross --locked
```

**Node.js >= 22.12** (for building the web UI):

```bash
# If you use nvm:
nvm install 22
nvm use 22
```

**SQLite** (for compile-time query validation):

```bash
# macOS
brew install sqlite3

# Linux
sudo apt install sqlite3
```

**First-time project setup:**

```bash
cd irrigation
make setup
```

This installs UI dependencies, validates tool versions, and creates the compile-time SQLite database that `sqlx` macros check against.

### Cross-compile for the Pis

Make sure Docker Desktop is running, then:

```bash
# Hub binary (Pi 5, aarch64, with real GPIO support)
make cross-hub

# Node binary (Pi Zero, armhf, with real ADS1115 ADC support)
make cross-node
```

The hub build takes a few minutes the first time (Docker pulls the cross-compilation image, builds the web UI, compiles the Rust workspace). Subsequent builds are faster.

The resulting binaries are at:

- `target/aarch64-unknown-linux-gnu/release/irrigation-hub`
- `target/arm-unknown-linux-gnueabihf/release/irrigation-node`

### Configure for monitor mode

Before deploying, edit `config.toml` to start in monitor mode. This way the hub will collect and display sensor data but will not actuate any valves — a safe starting point while you validate your sensor wiring and calibration.

```toml
mode = "monitor"
```

You'll switch to `"auto"` later, after confirming everything works.

### Deploy to the hub

The Makefile has deploy targets that cross-compile and scp in one step. You can also run the steps manually.

**Using Make (recommended):**

```bash
make deploy-hub
```

This compiles and copies the binary, config, and service file to `pi5.local`. Override the hostname if needed:

```bash
make deploy-hub HUB_HOST=192.168.1.50
```

**Then SSH into the hub to finish setup:**

```bash
ssh pi@pi5.local
```

Create the irrigation directory and install the systemd service:

```bash
mkdir -p ~/irrigation
sudo cp ~/irrigation-hub.service /etc/systemd/system/
```

Edit the service file to set your MQTT password (the one you created for `irrigation-hub` earlier):

```bash
sudo systemctl edit irrigation-hub --force
```

Add an override for the password:

```ini
[Service]
Environment=MQTT_PASS=your-actual-password-here
```

Install the Mosquitto production config:

```bash
sudo cp ~/mosquitto-production.conf /etc/mosquitto/conf.d/irrigation.conf
sudo systemctl restart mosquitto
```

Verify Mosquitto is listening with auth:

```bash
mosquitto_sub -h localhost -u irrigation-hub -P 'your-actual-password-here' -t '#' -v
```

You should see it connect and wait for messages (no error). Ctrl+C to exit.

Now start the hub:

```bash
sudo systemctl daemon-reload
sudo systemctl enable irrigation-hub
sudo systemctl start irrigation-hub
```

Check it's running:

```bash
sudo systemctl status irrigation-hub
journalctl -u irrigation-hub -f --no-pager
```

You should see the hub start up, connect to MQTT, create the database, and begin waiting for sensor readings.

Open the dashboard in your browser: `http://pi5.local:8080`

(This works from your local network because the hub binds to `127.0.0.1` by default — you'll need to be on the same machine or set up port forwarding / TLS for remote access. See the [Deployment Guide](deploy/README.md) for TLS setup.)

### Deploy to the nodes

Deploy to node A:

```bash
make deploy-node NODE_HOST=pizero-a.local
```

SSH in and set up the service:

```bash
ssh pi@pizero-a.local
sudo cp ~/irrigation-node.service /etc/systemd/system/
```

Edit the service file to set the correct `NODE_ID`, `MQTT_HOST`, and `MQTT_PASS`:

```bash
sudo systemctl edit irrigation-node --force
```

```ini
[Service]
Environment=NODE_ID=node-a
Environment=MQTT_HOST=pi5.local
Environment=MQTT_PASS=your-node-password-here
```

Enable and start:

```bash
sudo systemctl daemon-reload
sudo systemctl enable irrigation-node
sudo systemctl start irrigation-node
```

Verify:

```bash
journalctl -u irrigation-node -f --no-pager
```

You should see it connect to the MQTT broker and start publishing sensor readings.

**Deploy to node B:** Repeat the above with `NODE_HOST=pizero-b.local` and `NODE_ID=node-b`.

At this point, check the hub dashboard — you should see sensor readings arriving from both nodes. If you don't, jump to the troubleshooting section below.

---

## Part 6: Monitor Mode — Validate Your Sensors

With the system running in `mode = "monitor"`, the hub receives and stores sensor data but never opens a valve. This is the right place to spend a day or two validating your sensor setup before trusting the system to water autonomously.

### Calibrate your sensors

The `config.toml` ships with default calibration values:

```toml
raw_dry = 26000
raw_wet = 12000
```

These are raw ADC readings — the 16-bit value the ADS1115 returns. `raw_dry` is the reading when the sensor is in dry air. `raw_wet` is the reading when the sensor is submerged in water (or very wet soil). The hub converts raw readings to a 0.0–1.0 moisture percentage using these two values as endpoints.

The defaults are reasonable for DFRobot SEN0193 sensors at 3.3V, but your specific sensors will differ. Calibrate them:

**Step 1: Get the dry reading.**

Hold a sensor in open air (completely dry). Watch the raw readings on the dashboard or subscribe to the MQTT topic:

```bash
mosquitto_sub -h pi5.local -u irrigation-hub -P 'your-password' -t 'tele/+/reading' -v
```

You'll see JSON payloads like:

```json
{
  "ts": 1700000000,
  "readings": [
    { "sensor_id": "s1", "raw": 25800 },
    { "sensor_id": "s2", "raw": 26100 }
  ]
}
```

Note the raw values. Average a few readings. That's your `raw_dry`.

**Step 2: Get the wet reading.**

Submerge the sensor's flat sensing area (not the electronics end) in a glass of water. Wait for readings to stabilize (30–60 seconds). Note the raw values. That's your `raw_wet`.

**Step 3: Update config.toml.**

Replace the defaults with your measured values. Each sensor can have its own calibration:

```toml
[[sensors]]
sensor_id = "node-a/s1"
node_id = "node-a"
zone_id = "front-lawn"
raw_dry = 25800    # your measured value
raw_wet = 11500    # your measured value
```

Copy the updated config to the hub and restart:

```bash
scp config.toml pi@pi5.local:~/irrigation/config.toml
ssh pi@pi5.local sudo systemctl restart irrigation-hub
```

### What to look for

With calibrated sensors installed in soil, verify:

- **Readings appear every 5 minutes** on the dashboard (the default `SAMPLE_EVERY_S=300`).
- **Node status shows "online"** for both nodes.
- **Moisture percentages make physical sense.** A recently watered pot should read 50–80%. Dry soil should read 10–30%. If dry soil reads 95%, your `raw_dry` / `raw_wet` values are swapped or your wiring is wrong.
- **Different sensors show different values** (unless they're in identical soil). If two sensors on the same node show exactly the same reading, they may both be wired to the same ADS1115 channel.

Let it run for a day. Watch readings change as soil dries. Verify the system is stable — no crashes, no disconnects, no stale sensor warnings.

### Troubleshooting

**No readings on the dashboard:**

- Check MQTT connectivity: on the hub, run `mosquitto_sub -h localhost -u irrigation-hub -P 'your-password' -t 'tele/#' -v`. If nothing arrives, the nodes can't reach the broker.
- Check `MQTT_HOST` on the nodes — is it the hub's correct IP or hostname?
- Check firewall rules — port 1883 must be open on the hub.
- Check `journalctl -u irrigation-node -f` on each node for connection errors.

**Readings stuck at 0 or 32767:**

- I2C not enabled on the Pi Zero (`ls /dev/i2c-1` missing — run `raspi-config`).
- ADS1115 not detected — run `i2cdetect -y 1` and look for address `0x48`.
- ADDR pin not connected to GND (floating address).
- Wiring issue on SDA or SCL lines.

**Readings are identical across both sensors on a node:**

- Both sensor signal wires are connected to the same ADS1115 channel (AIN0). One should go to AIN0, the other to AIN1.

**Moisture percentage is inverted (wet reads as dry):**

- Your `raw_dry` and `raw_wet` values are swapped. `raw_dry` should be the higher number.

**Node keeps disconnecting and reconnecting:**

- Power supply issue — the Pi Zero is browning out. Try a different micro-USB cable (thin cables drop voltage under load) or a more capable power supply.

---

## Part 7: Auto Mode — Let It Water

Once your sensor readings are stable, calibrated, and making physical sense for at least a day or two, it's time to let the system water automatically.

### Enable auto mode

Edit `config.toml` and change the mode:

```toml
mode = "auto"
```

Review the zone parameters before deploying. Here's what each one controls:

```toml
[[zones]]
zone_id = "front-lawn"
name = "Front Lawn"
min_moisture = 0.3          # Below this → start watering
target_moisture = 0.5       # Stop watering when this is reached
pulse_sec = 30              # How long to open the valve per pulse
soak_min = 20               # How long to wait between pulses
max_open_sec_per_day = 180  # Daily safety limit: total valve-open seconds
max_pulses_per_day = 6      # Daily safety limit: total pulse count
stale_timeout_min = 30      # If no reading for this long, mark sensor stale
valve_gpio_pin = 17         # BCM GPIO pin for this zone's relay
```

Adjust these to match your setup. For a first run, conservative values are smart — shorter pulses, longer soak times, lower daily limits. You can tune them once you see how your soil and water source behave.

### How the watering algorithm works

The hub uses a pulse-and-soak strategy:

1. **Idle:** The hub monitors moisture. When the zone's average moisture drops below `min_moisture` (e.g., 30%), it transitions to watering.
2. **Watering:** The valve opens for `pulse_sec` seconds (e.g., 30s), then closes.
3. **Soaking:** The hub waits `soak_min` minutes (e.g., 20 min) for the water to percolate through the soil and reach the sensor. Soil moisture readings lag behind actual moisture — if you water continuously until the sensor reads "wet enough," you've massively overwatered.
4. **Re-evaluate:** After soaking, the hub checks the moisture reading again. If it's still below `target_moisture`, another pulse fires. If it's at or above target, the zone returns to idle.

This prevents runoff, overwatering, and oscillation. It's the same principle behind commercial drip irrigation controllers.

### Deploy and monitor

Copy the updated config and restart:

```bash
scp config.toml pi@pi5.local:~/irrigation/config.toml
ssh pi@pi5.local sudo systemctl restart irrigation-hub
```

Watch the logs during the first watering cycle:

```bash
ssh pi@pi5.local journalctl -u irrigation-hub -f --no-pager
```

You'll see log lines for:

- Scheduler state transitions (Idle → Watering → Soaking → Idle)
- Valve open/close events
- Daily safety counter updates

Also watch the dashboard — it shows the current scheduler state and valve status for each zone in real time.

### Safety limits

The system has multiple layers of protection against flooding:

- **Daily open-seconds cap** (`max_open_sec_per_day`): The total seconds a valve can be open in a calendar day. Once exceeded, no more watering until midnight.
- **Daily pulse cap** (`max_pulses_per_day`): Maximum watering pulses per day.
- **Stale sensor timeout** (`stale_timeout_min`): If a sensor hasn't reported in this many minutes, the hub marks it stale and won't water based on stale data.
- **Concurrent valve limit** (`max_concurrent_valves`): Prevents opening more valves than the 12V PSU can handle.
- **Normally-closed valves**: Power failure = water stops, regardless of software state.
- **Emergency shutdown**: The hub closes all valves on SIGTERM, SIGINT, and on any panic.

If the daily limits feel too restrictive after observing a few cycles, you can increase them. If they're triggering regularly, that's a signal your `min_moisture` is too high or your pulse duration is too short for your soil type — the system is fighting to reach `target_moisture` and running out of daily budget.

---

## Part 8: What's Next

You have a working irrigation system. Here are the natural next steps.

### Add more zones

Each new zone needs:

1. A solenoid valve wired to an unused relay channel on the hub.
2. A new `[[zones]]` entry in `config.toml` with a unique `zone_id` and `valve_gpio_pin`.
3. Sensor(s) mapped to the zone via `[[sensors]]` entries.

If you need more than 2 relay channels, swap your relay board for a 4-channel or 8-channel module. The valid GPIO pins for valve control are: 4, 5, 6, 12, 13, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27.

If you need more than 4 sensors per node, add a second ADS1115 with a different I2C address (connect ADDR to VDD for `0x49`, to SDA for `0x4A`, to SCL for `0x4B`). Set the address via the `ADS1115_ADDR` environment variable in the node service.

### Enable remote dashboard access

The web dashboard defaults to `127.0.0.1` (localhost only). For remote access, you need TLS to protect the API token in transit. Two options:

- **Native TLS**: Build with `make cross-hub-tls` and provide a self-signed certificate.
- **nginx reverse proxy**: Keep the hub on localhost and let nginx terminate TLS.

Both are documented in the [Deployment Guide](deploy/README.md).

### SD card longevity

The system is already configured to mitigate SD card wear — the database runs from tmpfs (RAM) with periodic backups to the SD card every 30 minutes. On unclean power loss, you lose at most 30 minutes of sensor readings (zone config is re-seeded from `config.toml` on every startup, so nothing structural is at risk).

If you want belt-and-suspenders reliability, attach a USB SSD to the Pi 5 and point the database there. See the [Deployment Guide](deploy/README.md) for instructions.

### Weatherproof for permanent installation

For outdoor deployment:

1. Mount the Pis on nylon standoffs inside the IP65 junction boxes.
2. Run all wires through PG7 cable glands — tighten until the rubber gasket compresses around the cable.
3. Seal any outdoor wire splices with adhesive-lined heat-shrink.
4. Mount enclosures in a shaded location — direct sun heats polycarbonate boxes well beyond the Pi's comfortable operating range.
5. Consider a small silica gel packet inside each enclosure to absorb condensation.
6. Bury sensor probes at root depth (5–15 cm for most garden plants) with the sensing end down and the electronics end above the soil line.

### Keep learning

- The [README](README.md) covers the full system architecture, safety features, and MQTT protocol in detail.
- The [Development Guide](DEVELOPMENT.md) has everything about local dev, Docker, Makefile targets, and environment variables.
- The [Deployment Guide](deploy/README.md) covers TLS, nginx, database management, and update procedures.

Happy growing.

---

Whoa, you actually made it this far? Amazing. How did it go?

Open an issue ticket if you've got general feedback, bugs,or a feature request.

If you want to connect or reach out with any questions, you can find me on [LinkedIn](https://www.linkedin.com/in/andrewalthage/).