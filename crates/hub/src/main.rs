mod db;
mod state;
mod web;

use anyhow::Result;
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use serde::Deserialize;
use std::{collections::HashMap, env, sync::Arc, time::Duration};
use tokio::sync::RwLock;
use tokio::time::sleep;

use db::{compute_moisture, Db, SensorConfig};
use state::{SensorReading, SystemState};

#[cfg(feature = "gpio")]
use rppal::gpio::{Gpio, OutputPin};

#[derive(Debug, Deserialize)]
struct Reading {
    sensor_id: String,
    raw: i64,
}

#[derive(Debug, Deserialize)]
struct ReadingMsg {
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
                pin.set_low(); // active-high relay OFF
            }

            pins.insert(zone_id.clone(), pin);
        }

        Ok(Self { pins, active_low })
    }

    fn set(&mut self, zone_id: &str, on: bool) {
        if let Some(pin) = self.pins.get_mut(zone_id) {
            if self.active_low {
                // active-low relay: LOW = ON, HIGH = OFF
                if on {
                    pin.set_low()
                } else {
                    pin.set_high()
                }
            } else {
                // active-high relay: HIGH = ON, LOW = OFF
                if on {
                    pin.set_high()
                } else {
                    pin.set_low()
                }
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

// ---------------------------------------------------------------------------
// Helpers (extracted for testability)
// ---------------------------------------------------------------------------

/// Extract node_id from "tele/<node_id>/reading".
fn extract_node_id(topic: &str) -> Option<&str> {
    let parts: Vec<&str> = topic.split('/').collect();
    if parts.len() == 3 && parts[0] == "tele" && parts[2] == "reading" {
        Some(parts[1])
    } else {
        None
    }
}

/// Extract zone_id from "valve/<zone_id>/set".
fn extract_zone_id(topic: &str) -> Option<&str> {
    let parts: Vec<&str> = topic.split('/').collect();
    if parts.len() == 3 && parts[0] == "valve" && parts[2] == "set" {
        Some(parts[1])
    } else {
        None
    }
}

/// Parse an "ON"/"OFF" payload into a bool (case-insensitive, trims whitespace).
fn parse_valve_command(payload: &[u8]) -> Result<bool, String> {
    let s = String::from_utf8_lossy(payload).trim().to_uppercase();
    match s.as_str() {
        "ON" => Ok(true),
        "OFF" => Ok(false),
        _ => Err(format!("unknown valve command '{s}'")),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // ── Env config ──────────────────────────────────────────────────
    let broker = env::var("MQTT_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port: u16 = env::var("MQTT_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1883);
    let db_url = env::var("DB_URL")
        .unwrap_or_else(|_| "sqlite:irrigation.db?mode=rwc".to_string());

    // ── Database ────────────────────────────────────────────────────
    let db = Db::connect(&db_url).await?;
    db.migrate().await?;

    // Load zone config from DB — this is the source of truth.
    let zones = db.load_zones().await?;
    if zones.is_empty() {
        eprintln!("WARNING: no zones configured in the database. \
                   Insert zones via the DB or a seed script.");
    }

    // Derive zone->GPIO mapping from persisted zone config.
    let zone_to_gpio: Vec<(String, u8)> = zones
        .iter()
        .map(|z| (z.zone_id.clone(), z.valve_gpio_pin as u8))
        .collect();

    // Build sensor lookup table for calibration during MQTT readings.
    let sensors = db.load_sensors().await?;
    let sensor_map: HashMap<String, SensorConfig> = sensors
        .into_iter()
        .map(|s| (s.sensor_id.clone(), s))
        .collect();

    eprintln!(
        "db ready — {} zone(s), {} sensor(s)",
        zones.len(),
        sensor_map.len()
    );

    // ── Valve board ─────────────────────────────────────────────────
    // Many common relay boards are active-low. If yours is active-high, set false.
    let active_low = env::var("RELAY_ACTIVE_LOW")
        .ok()
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(true);

    let mut valves = ValveBoard::new(&zone_to_gpio, active_low)?;
    valves.all_off();

    // ── Shared state (ephemeral, for the web UI) ────────────────────
    let shared = Arc::new(RwLock::new(SystemState::new(&zone_to_gpio)));
    {
        let mut st = shared.write().await;
        st.record_system("hub started".to_string());
    }

    // ── Web server ──────────────────────────────────────────────────
    let web_state = Arc::clone(&shared);
    let web_db = db.clone();
    tokio::spawn(async move {
        web::serve(web_state, web_db).await;
    });

    // ── MQTT ────────────────────────────────────────────────────────
    let client_id = "irrigation-hub";
    let mut mqttoptions = MqttOptions::new(client_id, broker, port);
    mqttoptions.set_keep_alive(Duration::from_secs(30));

    let (client, mut eventloop) = AsyncClient::new(mqttoptions, 20);

    client.subscribe("tele/+/reading", QoS::AtLeastOnce).await?;
    client.subscribe("valve/+/set", QoS::AtLeastOnce).await?;
    eprintln!("hub subscribed to tele/+/reading and valve/+/set");

    loop {
        match eventloop.poll().await {
            Ok(Event::Incoming(Packet::Publish(p))) => {
                let topic = p.topic.clone();
                let payload = p.payload.to_vec();

                if let Some(node_id) = extract_node_id(&topic) {
                    match serde_json::from_slice::<ReadingMsg>(&payload) {
                        Ok(msg) => {
                            let readings: Vec<SensorReading> = msg
                                .readings
                                .iter()
                                .map(|r| SensorReading {
                                    sensor_id: r.sensor_id.clone(),
                                    raw: r.raw,
                                })
                                .collect();

                            eprintln!(
                                "telemetry node={node_id} ts={} readings={:?}",
                                msg.ts, msg.readings
                            );

                            // Persist each reading to the DB (best-effort).
                            for r in &msg.readings {
                                if let Some(sc) = sensor_map.get(&r.sensor_id) {
                                    let moisture =
                                        compute_moisture(r.raw, sc.raw_dry, sc.raw_wet);
                                    if let Err(e) = db
                                        .insert_reading(msg.ts, &r.sensor_id, r.raw, moisture)
                                        .await
                                    {
                                        eprintln!(
                                            "db: insert_reading failed sensor={}: {e}",
                                            r.sensor_id
                                        );
                                    }
                                } else {
                                    eprintln!(
                                        "unknown sensor_id '{}' — skipping DB write",
                                        r.sensor_id
                                    );
                                }
                            }

                            let mut st = shared.write().await;
                            st.record_reading(node_id, readings);
                        }
                        Err(e) => {
                            eprintln!("bad telemetry json: {e} topic={topic}");
                            let mut st = shared.write().await;
                            st.record_error(format!("bad telemetry json: {e}"));
                        }
                    }
                } else if let Some(zone_id) = extract_zone_id(&topic) {
                    match parse_valve_command(&payload) {
                        Ok(on) => {
                            valves.set(zone_id, on);

                            // Track daily pulse count when valve turns ON.
                            if on {
                                let today = Db::today_yyyy_mm_dd();
                                if let Err(e) = db.add_pulse(&today, zone_id, 1).await {
                                    eprintln!("db: add_pulse failed zone={zone_id}: {e}");
                                }
                            }
                            // TODO: track valve-open duration for add_open_seconds
                            // and insert_watering_event (requires start-time bookkeeping).

                            let mut st = shared.write().await;
                            st.record_valve(zone_id, on);
                        }
                        Err(msg) => {
                            eprintln!("{msg} (use ON/OFF)");
                            let mut st = shared.write().await;
                            st.record_error(msg);
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

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- extract_node_id ----------------------------------------------------

    #[test]
    fn extract_node_id_valid_topic() {
        assert_eq!(extract_node_id("tele/node-a/reading"), Some("node-a"));
    }

    #[test]
    fn extract_node_id_different_node() {
        assert_eq!(
            extract_node_id("tele/greenhouse-1/reading"),
            Some("greenhouse-1")
        );
    }

    #[test]
    fn extract_node_id_wrong_prefix() {
        assert_eq!(extract_node_id("foo/node-a/reading"), None);
    }

    #[test]
    fn extract_node_id_wrong_suffix() {
        assert_eq!(extract_node_id("tele/node-a/status"), None);
    }

    #[test]
    fn extract_node_id_too_few_segments() {
        assert_eq!(extract_node_id("tele/reading"), None);
    }

    #[test]
    fn extract_node_id_too_many_segments() {
        assert_eq!(extract_node_id("tele/node-a/sub/reading"), None);
    }

    #[test]
    fn extract_node_id_empty_string() {
        assert_eq!(extract_node_id(""), None);
    }

    // -- extract_zone_id ----------------------------------------------------

    #[test]
    fn extract_zone_id_valid_topic() {
        assert_eq!(extract_zone_id("valve/zone1/set"), Some("zone1"));
    }

    #[test]
    fn extract_zone_id_wrong_prefix() {
        assert_eq!(extract_zone_id("pump/zone1/set"), None);
    }

    #[test]
    fn extract_zone_id_wrong_suffix() {
        assert_eq!(extract_zone_id("valve/zone1/get"), None);
    }

    #[test]
    fn extract_zone_id_too_few_segments() {
        assert_eq!(extract_zone_id("valve/set"), None);
    }

    #[test]
    fn extract_zone_id_empty_string() {
        assert_eq!(extract_zone_id(""), None);
    }

    // -- parse_valve_command ------------------------------------------------

    #[test]
    fn parse_valve_command_on_uppercase() {
        assert_eq!(parse_valve_command(b"ON"), Ok(true));
    }

    #[test]
    fn parse_valve_command_off_uppercase() {
        assert_eq!(parse_valve_command(b"OFF"), Ok(false));
    }

    #[test]
    fn parse_valve_command_on_lowercase() {
        assert_eq!(parse_valve_command(b"on"), Ok(true));
    }

    #[test]
    fn parse_valve_command_off_mixed_case() {
        assert_eq!(parse_valve_command(b"oFf"), Ok(false));
    }

    #[test]
    fn parse_valve_command_with_whitespace() {
        assert_eq!(parse_valve_command(b"  ON  "), Ok(true));
        assert_eq!(parse_valve_command(b"\tOFF\n"), Ok(false));
    }

    #[test]
    fn parse_valve_command_garbage() {
        assert!(parse_valve_command(b"TOGGLE").is_err());
    }

    #[test]
    fn parse_valve_command_empty() {
        assert!(parse_valve_command(b"").is_err());
    }

    // -- ValveBoard (mock) --------------------------------------------------

    #[test]
    fn valve_board_new_registers_zones() {
        let zones = vec![("z1".to_string(), 17), ("z2".to_string(), 27)];
        let board = ValveBoard::new(&zones, true).unwrap();
        assert_eq!(board.zones.len(), 2);
    }

    #[test]
    fn valve_board_new_all_off() {
        let zones = vec![("z1".to_string(), 17)];
        let board = ValveBoard::new(&zones, true).unwrap();
        assert!(!board.zones["z1"]);
    }

    #[test]
    fn valve_board_set_on() {
        let zones = vec![("z1".to_string(), 17)];
        let mut board = ValveBoard::new(&zones, true).unwrap();
        board.set("z1", true);
        assert!(board.zones["z1"]);
    }

    #[test]
    fn valve_board_set_off() {
        let zones = vec![("z1".to_string(), 17)];
        let mut board = ValveBoard::new(&zones, true).unwrap();
        board.set("z1", true);
        board.set("z1", false);
        assert!(!board.zones["z1"]);
    }

    #[test]
    fn valve_board_all_off_resets_everything() {
        let zones = vec![("z1".to_string(), 17), ("z2".to_string(), 27)];
        let mut board = ValveBoard::new(&zones, true).unwrap();
        board.set("z1", true);
        board.set("z2", true);
        board.all_off();
        assert!(!board.zones["z1"]);
        assert!(!board.zones["z2"]);
    }

    #[test]
    fn valve_board_set_unknown_zone_does_not_panic() {
        let zones = vec![("z1".to_string(), 17)];
        let mut board = ValveBoard::new(&zones, true).unwrap();
        board.set("nonexistent", true); // should not panic
        assert_eq!(board.zones.len(), 1); // no new entry created
    }

    // -- ReadingMsg deserialization ------------------------------------------

    #[test]
    fn reading_msg_deserialize_valid() {
        let json = r#"{"ts":1700000000,"readings":[{"sensor_id":"s1","raw":20000}]}"#;
        let msg: ReadingMsg = serde_json::from_str(json).unwrap();
        assert_eq!(msg.ts, 1700000000);
        assert_eq!(msg.readings.len(), 1);
        assert_eq!(msg.readings[0].sensor_id, "s1");
        assert_eq!(msg.readings[0].raw, 20000);
    }

    #[test]
    fn reading_msg_deserialize_multiple_readings() {
        let json = r#"{"ts":1,"readings":[{"sensor_id":"a","raw":1},{"sensor_id":"b","raw":2}]}"#;
        let msg: ReadingMsg = serde_json::from_str(json).unwrap();
        assert_eq!(msg.readings.len(), 2);
    }

    #[test]
    fn reading_msg_deserialize_missing_field_fails() {
        // Missing "readings" field
        let json = r#"{"ts":1}"#;
        assert!(serde_json::from_str::<ReadingMsg>(json).is_err());
    }

    #[test]
    fn reading_msg_deserialize_extra_fields_ignored() {
        let json = r#"{"ts":1,"readings":[],"extra":"ignored"}"#;
        let msg: ReadingMsg = serde_json::from_str(json).unwrap();
        assert_eq!(msg.ts, 1);
        assert!(msg.readings.is_empty());
    }
}
