mod config;
mod db;
mod mqtt;
mod state;
mod valve;
mod web;

use anyhow::Result;
use rumqttc::{AsyncClient, Event, MqttOptions, Packet, QoS};
use std::{collections::HashMap, env, sync::Arc, time::Duration};
use tokio::sync::RwLock;
use tokio::time::sleep;

use db::{compute_moisture, Db, SensorConfig};
use mqtt::{extract_node_id, extract_zone_id, parse_valve_command, ReadingMsg};
use state::{SensorReading, SystemState};
use valve::ValveBoard;

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

    // ── Config file (seed zones + sensors) ───────────────────────────
    let config_path = env::var("CONFIG_PATH").unwrap_or_else(|_| "config.toml".to_string());
    let cfg = config::load(&config_path)?;
    config::apply(&cfg, &db).await?;

    // Load zone config from DB — this is the source of truth.
    let zones = db.load_zones().await?;
    if zones.is_empty() {
        eprintln!("WARNING: no zones configured in the database.");
    }

    // Derive zone->GPIO mapping from persisted zone config.
    let zone_to_gpio: Vec<(String, u8)> = zones
        .iter()
        .map(|z| (z.zone_id.clone(), z.valve_gpio_pin as u8))
        .collect();

    // Build sensor lookup table for calibration during MQTT readings.
    // Keys are qualified IDs: "node-a/s1", "node-b/s2", etc.
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
                            // Qualify sensor_id with node_id so each node's
                            // local channel names ("s1", "s2") become unique.
                            for r in &msg.readings {
                                let qualified_id =
                                    format!("{node_id}/{}", r.sensor_id);
                                if let Some(sc) = sensor_map.get(&qualified_id) {
                                    let moisture =
                                        compute_moisture(r.raw, sc.raw_dry, sc.raw_wet);
                                    if let Err(e) = db
                                        .insert_reading(
                                            msg.ts, &qualified_id, r.raw, moisture,
                                        )
                                        .await
                                    {
                                        eprintln!(
                                            "db: insert_reading failed sensor={qualified_id}: {e}",
                                        );
                                    }
                                } else {
                                    eprintln!(
                                        "unknown sensor '{qualified_id}' — skipping DB write",
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
