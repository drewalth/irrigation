mod state;
mod web;

use anyhow::Result;
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use serde::Deserialize;
use std::{collections::HashMap, env, sync::Arc, time::Duration};
use tokio::sync::RwLock;
use tokio::time::sleep;

use state::{SensorReading, SystemState};

#[cfg(feature = "gpio")]
use rppal::gpio::{Gpio, OutputPin};

#[derive(Debug, Deserialize)]
struct Reading {
    sensor_id: String,
    raw: i32,
}

#[derive(Debug, Deserialize)]
struct ReadingMsg {
    #[allow(dead_code)]
    ts: i64,
    readings: Vec<Reading>,
}

// ---------------------------------------------------------------------------
// Real GPIO valve board (production — requires rppal + Raspberry Pi hardware)
// ---------------------------------------------------------------------------
#[cfg(feature = "gpio")]
struct ValveBoard {
    pins: HashMap<String, OutputPin>, // zone_id -> GPIO pin
    active_low: bool,                 // many relay boards are active-low
}

#[cfg(feature = "gpio")]
impl ValveBoard {
    fn new(zone_to_gpio: &[(String, u8)], active_low: bool) -> Result<Self> {
        let gpio = Gpio::new()?;
        let mut pins = HashMap::new();

        for (zone_id, pin_num) in zone_to_gpio {
            let mut pin = gpio.get(*pin_num)?.into_output();

            // Fail-safe: ensure "OFF" at startup
            if active_low {
                pin.set_high(); // active-low relay OFF
            } else {
                pin.set_low();  // active-high relay OFF
            }

            pins.insert(zone_id.clone(), pin);
        }

        Ok(Self { pins, active_low })
    }

    fn set(&mut self, zone_id: &str, on: bool) {
        if let Some(pin) = self.pins.get_mut(zone_id) {
            if self.active_low {
                // active-low relay: LOW = ON, HIGH = OFF
                if on { pin.set_low() } else { pin.set_high() }
            } else {
                // active-high relay: HIGH = ON, LOW = OFF
                if on { pin.set_high() } else { pin.set_low() }
            }
            eprintln!("valve zone={zone_id} set {}", if on { "ON" } else { "OFF" });
        } else {
            eprintln!("unknown zone_id '{zone_id}'");
        }
    }

    fn all_off(&mut self) {
        let keys: Vec<String> = self.pins.keys().cloned().collect();
        for k in keys {
            self.set(&k, false);
        }
    }
}

// ---------------------------------------------------------------------------
// Mock valve board (development — no hardware, logs state to stderr)
// ---------------------------------------------------------------------------
#[cfg(not(feature = "gpio"))]
struct ValveBoard {
    zones: HashMap<String, bool>, // zone_id -> on/off state
}

#[cfg(not(feature = "gpio"))]
impl ValveBoard {
    fn new(zone_to_gpio: &[(String, u8)], _active_low: bool) -> Result<Self> {
        let mut zones = HashMap::new();
        for (zone_id, pin_num) in zone_to_gpio {
            eprintln!("[mock-gpio] registered zone={zone_id} (gpio {pin_num} — not wired)");
            zones.insert(zone_id.clone(), false);
        }
        eprintln!("[mock-gpio] valve board initialised (no hardware)");
        Ok(Self { zones })
    }

    fn set(&mut self, zone_id: &str, on: bool) {
        if let Some(state) = self.zones.get_mut(zone_id) {
            *state = on;
            eprintln!(
                "[mock-gpio] valve zone={zone_id} set {}",
                if on { "ON" } else { "OFF" }
            );
        } else {
            eprintln!("[mock-gpio] unknown zone_id '{zone_id}'");
        }
    }

    fn all_off(&mut self) {
        let keys: Vec<String> = self.zones.keys().cloned().collect();
        for k in keys {
            self.set(&k, false);
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Env config
    let broker = env::var("MQTT_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port: u16 = env::var("MQTT_PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(1883);

    // Define your zone->GPIO mapping here (BCM numbering)
    // Example: zone1 on GPIO17, zone2 on GPIO27
    let zone_to_gpio = vec![
        ("zone1".to_string(), 17),
        ("zone2".to_string(), 27),
    ];

    // Many common relay boards are active-low. If yours is active-high, set false.
    let active_low = env::var("RELAY_ACTIVE_LOW")
        .ok()
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(true);

    let mut valves = ValveBoard::new(&zone_to_gpio, active_low)?;
    valves.all_off();

    // Shared state for the web UI
    let shared = Arc::new(RwLock::new(SystemState::new(&zone_to_gpio)));
    {
        let mut st = shared.write().await;
        st.record_system("hub started".to_string());
    }

    // Spawn the web server
    let web_state = Arc::clone(&shared);
    tokio::spawn(async move {
        web::serve(web_state).await;
    });

    // MQTT setup
    let client_id = "irrigation-hub";
    let mut mqttoptions = MqttOptions::new(client_id, broker, port);
    mqttoptions.set_keep_alive(Duration::from_secs(30));

    let (client, mut eventloop) = AsyncClient::new(mqttoptions, 20);

    // Subscribe to telemetry and valve commands
    client.subscribe("tele/+/reading", QoS::AtLeastOnce).await?;
    client.subscribe("valve/+/set", QoS::AtLeastOnce).await?;
    eprintln!("hub subscribed to tele/+/reading and valve/+/set");

    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Packet::Publish(p))) => {
                let topic = p.topic.clone();
                let payload = p.payload.to_vec();

                if topic.starts_with("tele/") && topic.ends_with("/reading") {
                    match serde_json::from_slice::<ReadingMsg>(&payload) {
                        Ok(msg) => {
                            // node_id is the middle topic segment: tele/<node_id>/reading
                            let parts: Vec<&str> = topic.split('/').collect();
                            let node_id = parts.get(1).copied().unwrap_or("unknown");

                            let readings: Vec<SensorReading> = msg
                                .readings
                                .iter()
                                .map(|r| SensorReading {
                                    sensor_id: r.sensor_id.clone(),
                                    raw: r.raw,
                                })
                                .collect();

                            eprintln!("telemetry node={node_id} ts={} readings={:?}", msg.ts, msg.readings);

                            let mut st = shared.write().await;
                            st.record_reading(node_id, readings);
                        }
                        Err(e) => {
                            eprintln!("bad telemetry json: {e} topic={topic}");
                            let mut st = shared.write().await;
                            st.record_error(format!("bad telemetry json: {e}"));
                        }
                    }
                } else if topic.starts_with("valve/") && topic.ends_with("/set") {
                    // valve/<zone_id>/set
                    let parts: Vec<&str> = topic.split('/').collect();
                    let zone_id = parts.get(1).copied().unwrap_or("");

                    let s = String::from_utf8_lossy(&payload).trim().to_uppercase();
                    match s.as_str() {
                        "ON" => {
                            valves.set(zone_id, true);
                            let mut st = shared.write().await;
                            st.record_valve(zone_id, true);
                        }
                        "OFF" => {
                            valves.set(zone_id, false);
                            let mut st = shared.write().await;
                            st.record_valve(zone_id, false);
                        }
                        _ => {
                            eprintln!("unknown valve command '{s}' (use ON/OFF)");
                            let mut st = shared.write().await;
                            st.record_error(format!("unknown valve command '{s}'"));
                        }
                    }
                } else {
                    eprintln!("unhandled topic={topic}");
                }
            }
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                eprintln!("mqtt connected");
                let mut st = shared.write().await;
                st.mqtt_connected = true;
                st.record_system("mqtt connected".to_string());
            }
            Ok(Event::Incoming(Packet::Disconnect)) => {
                eprintln!("mqtt disconnected");
                let mut st = shared.write().await;
                st.mqtt_connected = false;
                st.record_system("mqtt disconnected".to_string());
            }
            Ok(_) => {}
            Err(e) => {
                eprintln!("mqtt error: {e}. reconnecting...");
                // Best-effort fail-safe: turn everything off on comms error
                valves.all_off();

                let mut st = shared.write().await;
                st.mqtt_connected = false;
                st.record_error(format!("mqtt error: {e}"));
                drop(st);

                sleep(Duration::from_secs(2)).await;
            }
        }
    }
}
