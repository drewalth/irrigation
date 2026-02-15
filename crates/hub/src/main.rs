//! Hub entry point: reads env/config, connects to MQTT and SQLite, wires up
//! the valve board, web server, and event loop.
//!
//! Safety features:
//! - Signal handler: SIGTERM/SIGINT → all valves off before exit
//! - MQTT re-subscribe on every reconnect
//! - Valve safety limits: max pulses/sec per day enforced before opening
//! - Valve watchdog: force-close valves open longer than pulse_sec + margin
//! - Sensor failure detection: skip implausible raw ADC readings
//! - Data retention: periodic pruning of old readings

mod config;
mod db;
mod mqtt;
mod state;
mod valve;
mod web;

use anyhow::Result;
use rumqttc::{AsyncClient, Event, LastWill, MqttOptions, Packet, QoS};
use std::{collections::HashMap, env, sync::Arc, time::Duration};
use tokio::sync::{Mutex, RwLock};
use tokio::time::Instant;
use tracing::{error, info, warn};

use db::{compute_moisture, is_reading_plausible, Db, SensorConfig, ZoneConfig};
use mqtt::{extract_node_id, extract_zone_id, parse_valve_command, ReadingMsg};
use state::{SensorReading, SystemState};
use valve::ValveBoard;

/// Margin (in seconds) added to a zone's `pulse_sec` for the watchdog timer.
const WATCHDOG_MARGIN_SEC: u64 = 30;

/// How often the watchdog checks for stuck-open valves.
const WATCHDOG_INTERVAL_SEC: u64 = 5;

/// Data retention pruning interval (6 hours).
const PRUNE_INTERVAL_SEC: u64 = 6 * 3600;

/// Default data retention period in days.
const RETENTION_DAYS: i64 = 90;

#[tokio::main]
async fn main() -> Result<()> {
    // ── Structured logging ──────────────────────────────────────────
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // ── Env config ──────────────────────────────────────────────────
    let broker = env::var("MQTT_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port: u16 = env::var("MQTT_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1883);
    let db_url = env::var("DB_URL").unwrap_or_else(|_| "sqlite:irrigation.db?mode=rwc".to_string());

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
        warn!("no zones configured in the database");
    }

    // Derive zone->GPIO mapping from persisted zone config.
    let zone_to_gpio: Vec<(String, u8)> = zones
        .iter()
        .map(|z| (z.zone_id.clone(), z.valve_gpio_pin as u8))
        .collect();

    // Build zone config lookup for safety limit enforcement + watchdog.
    let zone_configs: HashMap<String, ZoneConfig> =
        zones.into_iter().map(|z| (z.zone_id.clone(), z)).collect();

    // Build sensor lookup table for calibration during MQTT readings.
    let sensors = db.load_sensors().await?;
    let sensor_map: HashMap<String, SensorConfig> = sensors
        .into_iter()
        .map(|s| (s.sensor_id.clone(), s))
        .collect();

    info!(
        zones = zone_configs.len(),
        sensors = sensor_map.len(),
        "database ready"
    );

    // ── Valve board ─────────────────────────────────────────────────
    let active_low = env::var("RELAY_ACTIVE_LOW")
        .ok()
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(true);

    let valves = Arc::new(Mutex::new(ValveBoard::new(&zone_to_gpio, active_low)?));
    valves.lock().await.all_off();

    // Track when each valve was opened (for watchdog + duration accounting).
    let valve_opened_at: Arc<Mutex<HashMap<String, Instant>>> =
        Arc::new(Mutex::new(HashMap::new()));

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

    // ── Valve watchdog ──────────────────────────────────────────────
    {
        let wd_valves = Arc::clone(&valves);
        let wd_opened = Arc::clone(&valve_opened_at);
        let wd_shared = Arc::clone(&shared);
        let wd_zone_configs = zone_configs.clone();
        let wd_db = db.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(WATCHDOG_INTERVAL_SEC));
            loop {
                ticker.tick().await;

                let mut opened = wd_opened.lock().await;
                let mut to_close: Vec<(String, u64)> = Vec::new();

                for (zone_id, opened_time) in opened.iter() {
                    let elapsed_secs = opened_time.elapsed().as_secs();
                    let max_secs = wd_zone_configs
                        .get(zone_id)
                        .map(|z| z.pulse_sec as u64 + WATCHDOG_MARGIN_SEC)
                        .unwrap_or(60 + WATCHDOG_MARGIN_SEC);

                    if elapsed_secs > max_secs {
                        to_close.push((zone_id.clone(), elapsed_secs));
                    }
                }

                if to_close.is_empty() {
                    continue;
                }

                let mut board = wd_valves.lock().await;
                let mut st = wd_shared.write().await;

                for (zone_id, elapsed_secs) in &to_close {
                    warn!(
                        zone = %zone_id,
                        elapsed_secs,
                        "watchdog: force-closing valve open too long"
                    );
                    board.set(zone_id, false);
                    opened.remove(zone_id.as_str());
                    st.record_valve(zone_id, false);
                    st.record_error(format!(
                        "watchdog force-closed valve {zone_id} after {elapsed_secs}s"
                    ));

                    // Record the open duration in daily counters
                    let today = Db::today_yyyy_mm_dd();
                    if let Err(e) = wd_db
                        .add_open_seconds(&today, zone_id, *elapsed_secs as i64)
                        .await
                    {
                        error!(zone = %zone_id, "watchdog: add_open_seconds failed: {e}");
                    }
                }
            }
        });
    }

    // ── Data retention pruning ──────────────────────────────────────
    {
        let prune_db = db.clone();
        tokio::spawn(async move {
            // Don't prune immediately on startup — wait a bit first.
            tokio::time::sleep(Duration::from_secs(60)).await;

            let mut ticker = tokio::time::interval(Duration::from_secs(PRUNE_INTERVAL_SEC));
            loop {
                ticker.tick().await;
                match prune_db.prune_old_readings(RETENTION_DAYS).await {
                    Ok(n) if n > 0 => info!(deleted = n, "pruned old readings"),
                    Ok(_) => {}
                    Err(e) => error!("data retention prune failed: {e:#}"),
                }
            }
        });
    }

    // ── MQTT ────────────────────────────────────────────────────────
    let client_id = "irrigation-hub";
    let mut mqttoptions = MqttOptions::new(client_id, &broker, port);
    mqttoptions.set_keep_alive(Duration::from_secs(30));
    mqttoptions.set_clean_session(false);
    mqttoptions.set_last_will(LastWill::new(
        "status/hub",
        b"offline".to_vec(),
        QoS::AtLeastOnce,
        true,
    ));

    let (client, mut eventloop) = AsyncClient::new(mqttoptions, 20);

    // Initial subscriptions (re-issued on every reconnect in ConnAck handler).
    client.subscribe("tele/+/reading", QoS::AtLeastOnce).await?;
    client.subscribe("valve/+/set", QoS::AtLeastOnce).await?;
    info!("subscribed to tele/+/reading and valve/+/set");

    // ── Signal handling ─────────────────────────────────────────────
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    // ── Main event loop ─────────────────────────────────────────────
    let exit_reason: &str;

    loop {
        tokio::select! {
            event = eventloop.poll() => {
                match event {
                    Ok(Event::Incoming(Packet::Publish(p))) => {
                        let topic = p.topic.clone();
                        let payload = p.payload.to_vec();

                        if let Some(node_id) = extract_node_id(&topic) {
                            handle_telemetry(
                                node_id,
                                &payload,
                                &sensor_map,
                                &db,
                                &shared,
                            )
                            .await;
                        } else if let Some(zone_id) = extract_zone_id(&topic) {
                            handle_valve_command(
                                zone_id,
                                &payload,
                                &zone_configs,
                                &valves,
                                &valve_opened_at,
                                &db,
                                &shared,
                            )
                            .await;
                        } else {
                            warn!(topic = %topic, "unhandled topic");
                        }
                    }

                    Ok(Event::Incoming(Packet::ConnAck(_))) => {
                        info!("mqtt connected");

                        // Re-subscribe on every (re)connect — broker may have
                        // lost our session even with clean_session(false).
                        if let Err(e) = client
                            .subscribe("tele/+/reading", QoS::AtLeastOnce)
                            .await
                        {
                            error!("re-subscribe tele/+/reading failed: {e}");
                        }
                        if let Err(e) = client
                            .subscribe("valve/+/set", QoS::AtLeastOnce)
                            .await
                        {
                            error!("re-subscribe valve/+/set failed: {e}");
                        }

                        // Announce online status (retained)
                        let _ = client
                            .publish("status/hub", QoS::AtLeastOnce, true, b"online".to_vec())
                            .await;

                        let mut st = shared.write().await;
                        st.mqtt_connected = true;
                        st.record_system("mqtt connected".to_string());
                    }

                    Ok(Event::Incoming(Packet::Disconnect)) => {
                        warn!("mqtt disconnected");
                        let mut st = shared.write().await;
                        st.mqtt_connected = false;
                        st.record_system("mqtt disconnected".to_string());
                    }

                    Ok(_) => {}

                    Err(e) => {
                        error!("mqtt error: {e} — turning all valves off");
                        emergency_all_off(
                            &valves,
                            &valve_opened_at,
                            &shared,
                            &format!("mqtt error: {e}"),
                        )
                        .await;
                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
            }

            _ = &mut ctrl_c => {
                exit_reason = "SIGINT";
                break;
            }

            _ = sigterm.recv() => {
                exit_reason = "SIGTERM";
                break;
            }
        }
    }

    // ── Graceful shutdown ───────────────────────────────────────────
    warn!(
        signal = exit_reason,
        "shutting down — turning all valves off"
    );
    emergency_all_off(
        &valves,
        &valve_opened_at,
        &shared,
        &format!("shutdown: {exit_reason}"),
    )
    .await;

    // Best-effort offline announcement before exit.
    let _ = client
        .publish("status/hub", QoS::AtLeastOnce, true, b"offline".to_vec())
        .await;

    info!("shutdown complete");
    Ok(())
}

// ---------------------------------------------------------------------------
// Telemetry handling (with sensor failure detection)
// ---------------------------------------------------------------------------

async fn handle_telemetry(
    node_id: &str,
    payload: &[u8],
    sensor_map: &HashMap<String, SensorConfig>,
    db: &Db,
    shared: &RwLock<SystemState>,
) {
    let msg: ReadingMsg = match serde_json::from_slice(payload) {
        Ok(m) => m,
        Err(e) => {
            warn!(node = %node_id, "bad telemetry json: {e}");
            let mut st = shared.write().await;
            st.record_error(format!("bad telemetry json from {node_id}: {e}"));
            return;
        }
    };

    let mut valid_readings: Vec<SensorReading> = Vec::new();

    for r in &msg.readings {
        let qualified_id = format!("{node_id}/{}", r.sensor_id);

        let Some(sc) = sensor_map.get(&qualified_id) else {
            warn!(sensor = %qualified_id, "unknown sensor — skipping DB write");
            continue;
        };

        // ── Sensor failure detection ────────────────────────────
        if !is_reading_plausible(r.raw, sc.raw_dry, sc.raw_wet) {
            warn!(
                sensor = %qualified_id,
                raw = r.raw,
                raw_dry = sc.raw_dry,
                raw_wet = sc.raw_wet,
                "implausible reading — possible sensor failure, skipping"
            );
            let mut st = shared.write().await;
            st.record_error(format!(
                "sensor {qualified_id} implausible raw={} (dry={}, wet={})",
                r.raw, sc.raw_dry, sc.raw_wet
            ));
            continue;
        }

        let moisture = compute_moisture(r.raw, sc.raw_dry, sc.raw_wet);
        if let Err(e) = db
            .insert_reading(msg.ts, &qualified_id, r.raw, moisture)
            .await
        {
            error!(sensor = %qualified_id, "insert_reading failed: {e}");
        }

        valid_readings.push(SensorReading {
            sensor_id: r.sensor_id.clone(),
            raw: r.raw,
        });
    }

    if !valid_readings.is_empty() {
        info!(
            node = %node_id,
            ts = msg.ts,
            count = valid_readings.len(),
            "telemetry received"
        );
        let mut st = shared.write().await;
        st.record_reading(node_id, valid_readings);
    }
}

// ---------------------------------------------------------------------------
// Valve command handling (with safety limit enforcement)
// ---------------------------------------------------------------------------

async fn handle_valve_command(
    zone_id: &str,
    payload: &[u8],
    zone_configs: &HashMap<String, ZoneConfig>,
    valves: &Mutex<ValveBoard>,
    valve_opened_at: &Mutex<HashMap<String, Instant>>,
    db: &Db,
    shared: &RwLock<SystemState>,
) {
    let on = match parse_valve_command(payload) {
        Ok(v) => v,
        Err(msg) => {
            warn!(zone = %zone_id, "{msg} (expected ON/OFF)");
            let mut st = shared.write().await;
            st.record_error(msg);
            return;
        }
    };

    if on {
        // ── Safety limit enforcement ────────────────────────────
        let today = Db::today_yyyy_mm_dd();
        let mut blocked = false;

        if let Some(zone_cfg) = zone_configs.get(zone_id) {
            match db.get_daily_counters(&today, zone_id).await {
                Ok(counters) => {
                    if counters.pulses >= zone_cfg.max_pulses_per_day {
                        warn!(
                            zone = %zone_id,
                            pulses = counters.pulses,
                            limit = zone_cfg.max_pulses_per_day,
                            "safety limit: max pulses/day reached — ignoring ON"
                        );
                        let mut st = shared.write().await;
                        st.record_error(format!(
                            "zone {zone_id}: ON blocked — {}/{} pulses today",
                            counters.pulses, zone_cfg.max_pulses_per_day
                        ));
                        blocked = true;
                    }
                    if !blocked && counters.open_sec >= zone_cfg.max_open_sec_per_day {
                        warn!(
                            zone = %zone_id,
                            open_sec = counters.open_sec,
                            limit = zone_cfg.max_open_sec_per_day,
                            "safety limit: max open sec/day reached — ignoring ON"
                        );
                        let mut st = shared.write().await;
                        st.record_error(format!(
                            "zone {zone_id}: ON blocked — {}s/{}s open today",
                            counters.open_sec, zone_cfg.max_open_sec_per_day
                        ));
                        blocked = true;
                    }
                }
                Err(e) => {
                    // If we can't check limits, allow the valve ON but log loudly.
                    error!(
                        zone = %zone_id,
                        "failed to check daily counters: {e} — allowing valve ON"
                    );
                }
            }
        }

        if !blocked {
            valves.lock().await.set(zone_id, true);
            valve_opened_at
                .lock()
                .await
                .insert(zone_id.to_string(), Instant::now());

            // Track daily pulse count.
            let today = Db::today_yyyy_mm_dd();
            if let Err(e) = db.add_pulse(&today, zone_id, 1).await {
                error!(zone = %zone_id, "add_pulse failed: {e}");
            }

            let mut st = shared.write().await;
            st.record_valve(zone_id, true);
        }
    } else {
        // ── Valve OFF ───────────────────────────────────────────
        valves.lock().await.set(zone_id, false);

        // Record open duration if we were tracking this valve.
        let mut opened = valve_opened_at.lock().await;
        if let Some(opened_time) = opened.remove(zone_id) {
            let duration_secs = opened_time.elapsed().as_secs() as i64;
            drop(opened); // release lock before DB calls

            let today = Db::today_yyyy_mm_dd();
            if let Err(e) = db.add_open_seconds(&today, zone_id, duration_secs).await {
                error!(zone = %zone_id, "add_open_seconds failed: {e}");
            }

            // Record watering event.
            let now_ts = now_unix();
            let start_ts = now_ts - duration_secs;
            if let Err(e) = db
                .insert_watering_event(start_ts, now_ts, zone_id, "mqtt_command", "ok")
                .await
            {
                error!(zone = %zone_id, "insert_watering_event failed: {e}");
            }

            info!(
                zone = %zone_id,
                duration_secs,
                "valve closed — duration recorded"
            );
        } else {
            drop(opened);
        }

        let mut st = shared.write().await;
        st.record_valve(zone_id, false);
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Turn all valves off, clear tracking state, and update system state.
async fn emergency_all_off(
    valves: &Mutex<ValveBoard>,
    valve_opened_at: &Mutex<HashMap<String, Instant>>,
    shared: &RwLock<SystemState>,
    reason: &str,
) {
    valves.lock().await.all_off();
    valve_opened_at.lock().await.clear();
    let mut st = shared.write().await;
    st.mqtt_connected = false;
    st.set_all_zones_off();
    st.record_error(format!("all valves off: {reason}"));
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
