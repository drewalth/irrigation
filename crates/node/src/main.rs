//! Sensor node: periodically publishes soil moisture readings over MQTT.
//!
//! With the `sim` feature (default) the node generates realistic fake sensor
//! data for local development.  With the `adc` feature, reads a real ADS1115
//! ADC over I2C (Pi Zero W production).

#[cfg(feature = "sim")]
mod sim;

#[cfg(feature = "adc")]
mod adc;

// Fail at compile time if no sensor backend is enabled.
#[cfg(not(any(feature = "sim", feature = "adc")))]
compile_error!("Enable either `sim` (fake data) or `adc` (real ADS1115) feature");

// Fail at compile time if both backends are enabled — they both define
// `let readings` in the sampling loop and would conflict.
#[cfg(all(feature = "sim", feature = "adc"))]
compile_error!("Features `sim` and `adc` are mutually exclusive");

use rumqttc::{AsyncClient, Event, LastWill, MqttOptions, Packet, QoS};
use serde::Serialize;
use std::{env, time::Duration};
use tokio::time::sleep;

#[derive(Debug, Serialize)]
struct Reading {
    sensor_id: String,
    raw: i32,
}

#[derive(Debug, Serialize)]
struct ReadingMsg {
    ts: i64,
    readings: Vec<Reading>,
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    // Structured logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // ── Env config ───────────────────────────────────────────────────
    let broker = env::var("MQTT_HOST").unwrap_or_else(|_| "192.168.1.10".to_string());
    let port: u16 = env::var("MQTT_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1883);
    let node_id = env::var("NODE_ID").unwrap_or_else(|_| "node-a".to_string());

    let sample_every_s: u64 = env::var("SAMPLE_EVERY_S")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(300);

    // ── Simulation config (only when `sim` feature is enabled) ───────
    #[cfg(feature = "sim")]
    let scenario = {
        let s = env::var("SIM_SCENARIO").unwrap_or_else(|_| "drying".to_string());
        sim::Scenario::from_str_lossy(&s)
    };
    #[cfg(feature = "sim")]
    let sim_raw_dry: f64 = env::var("SIM_RAW_DRY")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(26000.0);
    #[cfg(feature = "sim")]
    let sim_raw_wet: f64 = env::var("SIM_RAW_WET")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(12000.0);
    #[cfg(feature = "sim")]
    let sim_diurnal_period_s: f64 = env::var("SIM_DIURNAL_PERIOD_S")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600.0);
    #[cfg(feature = "sim")]
    let sim_zone_id: Option<String> = env::var("SIM_ZONE_ID").ok();

    #[cfg(feature = "sim")]
    let mut sim = sim::SoilMoistureSim::new(
        scenario,
        2, // two sensor channels: s1, s2
        sim_raw_dry,
        sim_raw_wet,
        sim_diurnal_period_s,
    );
    #[cfg(feature = "sim")]
    tracing::info!(
        scenario = %scenario,
        raw_dry = sim_raw_dry,
        raw_wet = sim_raw_wet,
        diurnal_period_s = sim_diurnal_period_s,
        zone_id = ?sim_zone_id,
        "simulation initialised"
    );

    // Watering flag shared between MQTT event loop and sampling loop.
    // The event loop sets it; the sampling loop reads it.  AtomicBool is
    // sufficient — no mutex needed.
    #[cfg(feature = "sim")]
    let watering_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // ── ADC config (only when `adc` feature is enabled) ──────────────
    #[cfg(feature = "adc")]
    let adc_addr: u16 = env::var("ADS1115_ADDR")
        .ok()
        .and_then(|s| u16::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0x48);

    #[cfg(feature = "adc")]
    let adc_channels = {
        let raw = env::var("SENSOR_CHANNELS").unwrap_or_default();
        adc::parse_channels(&raw)?
    };

    #[cfg(feature = "adc")]
    let mut adc_device = adc::Ads1115::new(adc_addr, adc_channels)?;

    // ── MQTT setup ───────────────────────────────────────────────────
    let client_id = format!("irrigation-node-{}", node_id);
    let status_topic = format!("status/node/{}", node_id);

    let mut mqttoptions = MqttOptions::new(client_id, broker, port);
    mqttoptions.set_keep_alive(Duration::from_secs(30));

    // Last Will Testament: broker publishes "offline" (retained) if the node
    // disconnects ungracefully.  The hub subscribes to status/node/+ to track
    // which nodes are alive.
    mqttoptions.set_last_will(LastWill::new(
        &status_topic,
        b"offline".to_vec(),
        QoS::AtLeastOnce,
        true,
    ));

    // MQTT authentication — required for production (see deploy/mosquitto-production.conf).
    if let (Ok(user), Ok(pass)) = (env::var("MQTT_USER"), env::var("MQTT_PASS")) {
        mqttoptions.set_credentials(user, pass);
        tracing::info!("mqtt: using password authentication");
    } else {
        tracing::warn!("MQTT_USER / MQTT_PASS not set — connecting without authentication");
    }

    let (client, mut eventloop) = AsyncClient::new(mqttoptions, 10);

    // ── MQTT event loop task ─────────────────────────────────────────
    let status_client = client.clone();
    let el_status_topic = status_topic.clone();

    // Build the valve subscription topic if SIM_ZONE_ID is set.
    #[cfg(feature = "sim")]
    let valve_topic: Option<String> = sim_zone_id.as_ref().map(|z| format!("valve/{z}/set"));

    #[cfg(feature = "sim")]
    let el_valve_topic = valve_topic.clone();
    #[cfg(feature = "sim")]
    let el_watering_flag = watering_flag.clone();

    tokio::spawn(async move {
        loop {
            match eventloop.poll().await {
                Ok(Event::Incoming(Packet::ConnAck(_))) => {
                    tracing::info!("node connected to mqtt");

                    // Announce online (retained) — mirrors the LWT "offline".
                    if let Err(e) = status_client
                        .publish(&el_status_topic, QoS::AtLeastOnce, true, b"online".to_vec())
                        .await
                    {
                        tracing::error!("failed to publish online status: {e}");
                    }

                    // Subscribe to valve commands for watering response.
                    #[cfg(feature = "sim")]
                    if let Some(ref vt) = el_valve_topic {
                        if let Err(e) = status_client.subscribe(vt, QoS::AtLeastOnce).await {
                            tracing::error!("failed to subscribe to {vt}: {e}");
                        } else {
                            tracing::info!(topic = %vt, "subscribed to valve commands");
                        }
                    }
                }

                // Handle incoming valve commands (sim only).
                #[cfg(feature = "sim")]
                Ok(Event::Incoming(Packet::Publish(pub_msg))) => {
                    if let Some(ref vt) = el_valve_topic {
                        if pub_msg.topic == *vt {
                            let payload =
                                std::str::from_utf8(&pub_msg.payload).unwrap_or("").trim();
                            match payload {
                                "open" => {
                                    tracing::info!("sim: valve open — wetting sensors");
                                    el_watering_flag
                                        .store(true, std::sync::atomic::Ordering::Relaxed);
                                }
                                "close" => {
                                    tracing::info!("sim: valve closed — resuming drying");
                                    el_watering_flag
                                        .store(false, std::sync::atomic::Ordering::Relaxed);
                                }
                                other => {
                                    tracing::debug!(
                                        payload = other,
                                        "ignoring unknown valve payload"
                                    );
                                }
                            }
                        }
                    }
                }

                Ok(_) => {}
                Err(e) => {
                    tracing::error!("mqtt error: {e} — retrying");
                    sleep(Duration::from_secs(2)).await;
                }
            }
        }
    });

    // ── Sampling loop ────────────────────────────────────────────────
    let topic = format!("tele/{}/reading", node_id);
    tracing::info!(topic = %topic, "publishing sensor readings");

    loop {
        // Produce readings from the active sensor backend.
        #[cfg(feature = "sim")]
        let readings: Vec<Reading> = {
            let watering = watering_flag.load(std::sync::atomic::Ordering::Relaxed);
            sim.set_watering(watering);

            let mut out = Vec::with_capacity(sim.sensor_count());
            for i in 0..sim.sensor_count() {
                out.push(Reading {
                    sensor_id: format!("s{}", i + 1),
                    raw: sim.sample(i),
                });
            }
            out
        };

        #[cfg(feature = "adc")]
        let readings: Vec<Reading> = adc_device.read_all();

        // Publish if we got at least one reading from whichever backend.
        if !readings.is_empty() {
            let msg = ReadingMsg {
                ts: now_unix(),
                readings,
            };

            let payload = serde_json::to_vec(&msg).expect("reading serialization failed");

            if let Err(e) = client
                .publish(&topic, QoS::AtLeastOnce, false, payload)
                .await
            {
                tracing::error!("publish error: {e}");
            } else {
                tracing::info!(ts = msg.ts, "published readings");
            }
        } else {
            tracing::warn!("no readings produced — skipping publish");
        }

        sleep(Duration::from_secs(sample_every_s)).await;
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_unix_returns_positive() {
        assert!(now_unix() > 0);
    }

    #[test]
    fn now_unix_is_recent() {
        let ts = now_unix();
        // Should be after 2024-01-01 (1704067200) and before 2040-01-01 (2208988800)
        assert!(ts > 1_704_067_200, "timestamp too old: {ts}");
        assert!(ts < 2_208_988_800, "timestamp too far in future: {ts}");
    }

    #[test]
    fn reading_msg_serializes_to_valid_json() {
        let msg = ReadingMsg {
            ts: 1_700_000_000,
            readings: vec![
                Reading {
                    sensor_id: "s1".to_string(),
                    raw: 20000,
                },
                Reading {
                    sensor_id: "s2".to_string(),
                    raw: 21000,
                },
            ],
        };
        let json = serde_json::to_value(&msg).unwrap();

        assert_eq!(json["ts"], 1_700_000_000);
        assert!(json["readings"].is_array());
        assert_eq!(json["readings"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn reading_serializes_with_correct_fields() {
        let r = Reading {
            sensor_id: "adc0".to_string(),
            raw: 12345,
        };
        let json = serde_json::to_value(&r).unwrap();

        assert_eq!(json["sensor_id"], "adc0");
        assert_eq!(json["raw"], 12345);
        // Should have exactly these two fields, no extras
        assert_eq!(json.as_object().unwrap().len(), 2);
    }
}
