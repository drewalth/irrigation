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
mod scheduler;
mod state;
mod valve;
mod web;

use anyhow::{Context, Result};
use rumqttc::{AsyncClient, Event, LastWill, MqttOptions, Packet, QoS};
use std::{
    collections::{HashMap, HashSet},
    env,
    sync::Arc,
    time::Duration,
};
use time::OffsetDateTime;
use tokio::sync::{Mutex, RwLock};
use tokio::time::Instant;
use tracing::{error, info, warn};

use config::OperationMode;
use db::{compute_moisture, is_reading_plausible, Db, SensorConfig, ZoneConfig};
use mqtt::{
    extract_node_id, extract_node_status_id, extract_zone_id, parse_valve_command, ReadingMsg,
};
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

/// Grace period (seconds) for MQTT errors before triggering emergency valve
/// shutdown.  During this window the hub logs warnings but does not interrupt
/// active watering sessions.  The valve watchdog still independently enforces
/// max-open-time safety regardless of MQTT state.
const MQTT_GRACE_PERIOD_SEC: u64 = 60;

/// How often the heartbeat monitor checks for stale nodes (seconds).
const HEARTBEAT_CHECK_INTERVAL_SEC: u64 = 60;

/// Default threshold (in minutes) after which a node is considered stale if no
/// telemetry has been received.  Override with `NODE_STALE_TIMEOUT_MIN` env var.
/// Should be roughly 2× the node sampling interval (default 300s = 5 min).
const DEFAULT_NODE_STALE_TIMEOUT_MIN: i64 = 10;

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
    let db_backup_path = env::var("DB_BACKUP_PATH").ok().filter(|s| !s.is_empty());
    let db_backup_interval: u64 = env::var("DB_BACKUP_INTERVAL_SEC")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1800);

    // ── Database ────────────────────────────────────────────────────
    // When using tmpfs the database file is lost on reboot.  Restore
    // from the persistent backup (if one exists) before connecting.
    if let (Some(working_path), Some(ref backup)) = (db::db_file_path(&db_url), &db_backup_path) {
        match db::restore_from_backup(&working_path, backup) {
            Ok(true) => info!(backup = %backup, "database restored from backup"),
            Ok(false) => {}
            Err(e) => warn!("backup restore failed (starting fresh): {e:#}"),
        }
    }

    let db = Db::connect(&db_url).await?;
    db.migrate().await?;

    // ── Config file (seed zones + sensors) ───────────────────────────
    let config_path = env::var("CONFIG_PATH").unwrap_or_else(|_| "config.toml".to_string());
    let cfg = config::load(&config_path)?;
    config::apply(&cfg, &db).await?;
    let max_concurrent_valves = cfg.max_concurrent_valves;
    let mode = cfg.mode;
    info!(?mode, "operation mode");

    // Load zone config from DB — this is the source of truth.
    let zones = db.load_zones().await?;
    if zones.is_empty() {
        warn!("no zones configured in the database");
    }

    // Derive zone->GPIO mapping from persisted zone config.
    let zone_to_gpio: Vec<(String, u8)> = if mode == OperationMode::Monitor {
        // Monitor mode: no GPIO pins claimed.
        Vec::new()
    } else {
        zones
            .iter()
            .map(|z| {
                let pin: u8 = z.valve_gpio_pin.try_into().with_context(|| {
                    format!(
                        "zone '{}': valve_gpio_pin {} out of u8 range",
                        z.zone_id, z.valve_gpio_pin
                    )
                })?;
                Ok((z.zone_id.clone(), pin))
            })
            .collect::<Result<Vec<_>>>()?
    };

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
    let mode_str = match mode {
        OperationMode::Auto => "auto",
        OperationMode::Monitor => "monitor",
    };
    let shared = Arc::new(RwLock::new(SystemState::new(&zone_to_gpio, mode_str)));
    {
        let mut st = shared.write().await;
        st.record_system("hub started".to_string());
    }

    // ── Web server ──────────────────────────────────────────────────
    let web_state = Arc::clone(&shared);
    let web_db = db.clone();
    let mut web_handle = tokio::spawn(async move {
        web::serve(web_state, web_db).await;
    });

    // ── Valve watchdog ──────────────────────────────────────────────
    let mut watchdog_handle = if mode == OperationMode::Monitor {
        tokio::spawn(async { std::future::pending::<()>().await })
    } else {
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
        })
    };

    // ── Data retention pruning ──────────────────────────────────────
    let mut prune_handle = {
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
        })
    };

    // ── Periodic database backup (SD card wear mitigation) ──────────
    let mut backup_handle = {
        let backup_db = db.clone();
        let backup_dest = db_backup_path.clone();
        tokio::spawn(async move {
            let Some(dest) = backup_dest else {
                // No backup path configured — park this task forever.
                std::future::pending::<()>().await;
                return;
            };
            info!(
                path = %dest,
                interval_sec = db_backup_interval,
                "database backup task started"
            );

            // Delay first backup to avoid startup I/O contention.
            tokio::time::sleep(Duration::from_secs(120)).await;

            let mut ticker = tokio::time::interval(Duration::from_secs(db_backup_interval));
            loop {
                ticker.tick().await;
                match backup_db.backup(&dest).await {
                    Ok(()) => info!(path = %dest, "database backup complete"),
                    Err(e) => error!("database backup failed: {e:#}"),
                }
            }
        })
    };

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

    // MQTT authentication — required for production (see deploy/mosquitto-production.conf).
    if let (Ok(user), Ok(pass)) = (env::var("MQTT_USER"), env::var("MQTT_PASS")) {
        mqttoptions.set_credentials(user, pass);
        info!("mqtt: using password authentication");
    } else {
        warn!("MQTT_USER / MQTT_PASS not set — connecting without authentication");
    }

    let (client, mut eventloop) = AsyncClient::new(mqttoptions, 20);

    // Initial subscriptions (re-issued on every reconnect in ConnAck handler).
    client.subscribe("tele/+/reading", QoS::AtLeastOnce).await?;
    client.subscribe("valve/+/set", QoS::AtLeastOnce).await?;
    client.subscribe("status/node/+", QoS::AtLeastOnce).await?;
    info!("subscribed to tele/+/reading, valve/+/set, status/node/+");

    // ── Auto-watering scheduler ─────────────────────────────────────
    let mut scheduler_handle = {
        let sched_db = db.clone();
        let sched_configs = zone_configs.clone();
        let sched_mqtt = client.clone();
        let sched_shared = Arc::clone(&shared);
        tokio::spawn(async move {
            scheduler::run(
                sched_db,
                sched_configs,
                sched_mqtt,
                sched_shared,
                max_concurrent_valves,
                mode,
            )
            .await;
        })
    };

    // ── Node heartbeat monitor ─────────────────────────────────────
    let mut heartbeat_handle = {
        let hb_shared = Arc::clone(&shared);
        tokio::spawn(async move {
            let stale_timeout_min: i64 = env::var("NODE_STALE_TIMEOUT_MIN")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_NODE_STALE_TIMEOUT_MIN);

            let stale_threshold = time::Duration::minutes(stale_timeout_min);

            info!(stale_timeout_min, "node heartbeat monitor started");

            // Wait before first check to let nodes connect and send initial data.
            tokio::time::sleep(Duration::from_secs(120)).await;

            let mut warned_stale: HashSet<String> = HashSet::new();
            let mut ticker =
                tokio::time::interval(Duration::from_secs(HEARTBEAT_CHECK_INTERVAL_SEC));

            loop {
                ticker.tick().await;

                let st = hb_shared.read().await;
                let now = OffsetDateTime::now_utc();

                let mut newly_stale: Vec<(String, i64)> = Vec::new();
                let mut recovered: Vec<String> = Vec::new();

                for (node_id, node) in &st.nodes {
                    let elapsed = now - node.last_seen;
                    let is_stale = elapsed > stale_threshold;

                    if is_stale && !warned_stale.contains(node_id) {
                        newly_stale.push((node_id.clone(), elapsed.whole_minutes()));
                    } else if !is_stale && warned_stale.contains(node_id) {
                        recovered.push(node_id.clone());
                    }
                }

                drop(st);

                if newly_stale.is_empty() && recovered.is_empty() {
                    continue;
                }

                let mut st = hb_shared.write().await;

                for (node_id, mins) in &newly_stale {
                    warn!(
                        node = %node_id,
                        last_seen_min_ago = mins,
                        "node is stale — no data received"
                    );
                    st.record_error(format!("node {node_id} stale — last seen {mins} min ago"));
                    warned_stale.insert(node_id.clone());
                }

                for node_id in &recovered {
                    info!(node = %node_id, "stale node recovered");
                    st.record_system(format!("node {node_id} recovered from stale state"));
                    warned_stale.remove(node_id);
                }
            }
        })
    };

    // ── Signal handling ─────────────────────────────────────────────
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    // ── Main event loop ─────────────────────────────────────────────
    let exit_reason: &str;

    // MQTT error grace period tracking (audit item #15).  Transient network
    // hiccups should not kill active watering sessions.
    let mut mqtt_first_error_at: Option<Instant> = None;
    let mut mqtt_error_count: u32 = 0;

    loop {
        tokio::select! {
            event = eventloop.poll() => {
                match event {
                    Ok(ev) => {
                        // Any incoming packet (except Disconnect) proves the
                        // broker is talking to us — clear the error streak.
                        let is_incoming = matches!(&ev, Event::Incoming(_));
                        let is_disconnect =
                            matches!(&ev, Event::Incoming(Packet::Disconnect));
                        if is_incoming
                            && !is_disconnect
                            && mqtt_first_error_at.is_some()
                        {
                            info!(
                                recovered_after_errors = mqtt_error_count,
                                "mqtt connection recovered — error streak cleared"
                            );
                            mqtt_first_error_at = None;
                            mqtt_error_count = 0;
                        }

                        match ev {
                            Event::Incoming(Packet::Publish(p)) => {
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
                                } else if let Some(zone_id) =
                                    extract_zone_id(&topic)
                                {
                                    handle_valve_command(
                                        zone_id,
                                        &payload,
                                        &zone_configs,
                                        &valves,
                                        &valve_opened_at,
                                        &db,
                                        &shared,
                                        max_concurrent_valves,
                                        mode,
                                    )
                                    .await;
                                } else if let Some(node_id) =
                                    extract_node_status_id(&topic)
                                {
                                    handle_node_status(
                                        node_id, &payload, &shared,
                                    )
                                    .await;
                                } else {
                                    warn!(topic = %topic, "unhandled topic");
                                }
                            }

                            Event::Incoming(Packet::ConnAck(_)) => {
                                info!("mqtt connected");

                                // Re-subscribe on every (re)connect — broker
                                // may have lost our session even with
                                // clean_session(false).
                                if let Err(e) = client
                                    .subscribe(
                                        "tele/+/reading",
                                        QoS::AtLeastOnce,
                                    )
                                    .await
                                {
                                    error!(
                                        "re-subscribe tele/+/reading failed: {e}"
                                    );
                                }
                                if let Err(e) = client
                                    .subscribe(
                                        "valve/+/set",
                                        QoS::AtLeastOnce,
                                    )
                                    .await
                                {
                                    error!(
                                        "re-subscribe valve/+/set failed: {e}"
                                    );
                                }
                                if let Err(e) = client
                                    .subscribe(
                                        "status/node/+",
                                        QoS::AtLeastOnce,
                                    )
                                    .await
                                {
                                    error!(
                                        "re-subscribe status/node/+ failed: {e}"
                                    );
                                }

                                // Announce online status (retained)
                                let _ = client
                                    .publish(
                                        "status/hub",
                                        QoS::AtLeastOnce,
                                        true,
                                        b"online".to_vec(),
                                    )
                                    .await;

                                let mut st = shared.write().await;
                                st.mqtt_connected = true;
                                st.record_system("mqtt connected".to_string());
                            }

                            Event::Incoming(Packet::Disconnect) => {
                                warn!("mqtt disconnected");
                                let mut st = shared.write().await;
                                st.mqtt_connected = false;
                                st.record_system(
                                    "mqtt disconnected".to_string(),
                                );
                            }

                            _ => {}
                        }
                    }

                    Err(e) => {
                        mqtt_error_count += 1;
                        let first_err =
                            *mqtt_first_error_at.get_or_insert_with(Instant::now);
                        let error_duration = first_err.elapsed();

                        // Mark MQTT disconnected on first error in a streak.
                        {
                            let mut st = shared.write().await;
                            if st.mqtt_connected {
                                st.mqtt_connected = false;
                                st.record_system(format!("mqtt error: {e}"));
                            }
                        }

                        // Only kill valves if they're open AND the grace period
                        // has expired.  The valve watchdog independently enforces
                        // max-open-time safety regardless of MQTT state.
                        let has_open_valves = {
                            let st = shared.read().await;
                            st.zones.values().any(|z| z.on)
                        };

                        if has_open_valves
                            && error_duration
                                >= Duration::from_secs(MQTT_GRACE_PERIOD_SEC)
                        {
                            error!(
                                consecutive_errors = mqtt_error_count,
                                elapsed_secs = error_duration.as_secs(),
                                "mqtt grace period expired with open valves \
                                 — emergency all-off"
                            );
                            emergency_all_off(
                                &valves,
                                &valve_opened_at,
                                &shared,
                                &format!(
                                    "mqtt error: {e} ({}s grace period expired, \
                                     {} consecutive errors)",
                                    MQTT_GRACE_PERIOD_SEC, mqtt_error_count
                                ),
                            )
                            .await;
                            // Reset — we already shut everything down.
                            mqtt_first_error_at = None;
                            mqtt_error_count = 0;
                        } else if has_open_valves {
                            let remaining = MQTT_GRACE_PERIOD_SEC
                                .saturating_sub(error_duration.as_secs());
                            warn!(
                                consecutive_errors = mqtt_error_count,
                                elapsed_secs = error_duration.as_secs(),
                                grace_remaining_secs = remaining,
                                "mqtt error with open valves \
                                 — grace period active: {e}"
                            );
                        } else {
                            warn!(
                                consecutive_errors = mqtt_error_count,
                                elapsed_secs = error_duration.as_secs(),
                                "mqtt error (no valves open): {e}"
                            );
                        }

                        tokio::time::sleep(Duration::from_secs(2)).await;
                    }
                }
            }

            // ── Critical task monitoring ──────────────────────────
            result = &mut watchdog_handle => {
                error!("CRITICAL: valve watchdog task exited unexpectedly: {result:?}");
                exit_reason = "watchdog task died";
                break;
            }

            result = &mut scheduler_handle => {
                error!("CRITICAL: scheduler task exited unexpectedly: {result:?}");
                exit_reason = "scheduler task died";
                break;
            }

            result = &mut web_handle => {
                error!("web server task exited unexpectedly: {result:?}");
                // Web server dying is not safety-critical; continue running.
                // Don't break — MQTT loop + scheduler + watchdog still work.
            }

            result = &mut prune_handle => {
                error!("data pruner task exited unexpectedly: {result:?}");
                // Not safety-critical; log and continue.
            }

            result = &mut backup_handle => {
                error!("database backup task exited unexpectedly: {result:?}");
                // Not safety-critical; log and continue.
            }

            result = &mut heartbeat_handle => {
                error!("node heartbeat monitor exited unexpectedly: {result:?}");
                // Not safety-critical; log and continue.
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

    // Final database backup before exit.
    if let Some(ref dest) = db_backup_path {
        info!("performing final database backup");
        match db.backup(dest).await {
            Ok(()) => info!(path = %dest, "final database backup complete"),
            Err(e) => error!("final database backup failed: {e:#}"),
        }
    }

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

/// Maximum MQTT telemetry payload size (4 KiB). Anything larger is likely
/// malicious or a bug — a normal reading message is a few hundred bytes.
const MAX_TELEMETRY_PAYLOAD_BYTES: usize = 4096;

/// Maximum number of sensor readings in a single telemetry message.
const MAX_READINGS_PER_MESSAGE: usize = 32;

async fn handle_telemetry(
    node_id: &str,
    payload: &[u8],
    sensor_map: &HashMap<String, SensorConfig>,
    db: &Db,
    shared: &RwLock<SystemState>,
) {
    if payload.len() > MAX_TELEMETRY_PAYLOAD_BYTES {
        warn!(
            node = %node_id,
            bytes = payload.len(),
            "telemetry payload too large — dropping"
        );
        let mut st = shared.write().await;
        st.record_error(format!(
            "telemetry from {node_id} dropped: {} bytes exceeds {} limit",
            payload.len(),
            MAX_TELEMETRY_PAYLOAD_BYTES
        ));
        return;
    }

    let msg: ReadingMsg = match serde_json::from_slice(payload) {
        Ok(m) => m,
        Err(e) => {
            warn!(node = %node_id, "bad telemetry json: {e}");
            let mut st = shared.write().await;
            st.record_error(format!("bad telemetry json from {node_id}: {e}"));
            return;
        }
    };

    if msg.readings.len() > MAX_READINGS_PER_MESSAGE {
        warn!(
            node = %node_id,
            count = msg.readings.len(),
            "too many readings in message — dropping"
        );
        let mut st = shared.write().await;
        st.record_error(format!(
            "telemetry from {node_id} dropped: {} readings exceeds {} limit",
            msg.readings.len(),
            MAX_READINGS_PER_MESSAGE
        ));
        return;
    }

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

#[allow(clippy::too_many_arguments)]
async fn handle_valve_command(
    zone_id: &str,
    payload: &[u8],
    zone_configs: &HashMap<String, ZoneConfig>,
    valves: &Mutex<ValveBoard>,
    valve_opened_at: &Mutex<HashMap<String, Instant>>,
    db: &Db,
    shared: &RwLock<SystemState>,
    max_concurrent_valves: usize,
    mode: OperationMode,
) {
    if mode == OperationMode::Monitor {
        warn!(zone = %zone_id, "valve command ignored — system is in monitor mode");
        let mut st = shared.write().await;
        st.record_error(format!(
            "valve command ignored for {zone_id} — system is in monitor mode"
        ));
        return;
    }

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
        // ── Concurrent valve limit ──────────────────────────────
        {
            let st = shared.read().await;
            let zone_already_on = st.zones.get(zone_id).is_some_and(|z| z.on);
            if !zone_already_on {
                let active = st.zones.values().filter(|z| z.on).count();
                if active >= max_concurrent_valves {
                    drop(st);
                    warn!(
                        zone = %zone_id,
                        active,
                        limit = max_concurrent_valves,
                        "concurrent valve limit reached — ignoring ON"
                    );
                    let mut st = shared.write().await;
                    st.record_error(format!(
                        "zone {zone_id}: ON blocked — {active}/{max_concurrent_valves} valves already open"
                    ));
                    return;
                }
            }
        }

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
            // Acquire both locks before opening to ensure the watchdog
            // sees the open timestamp atomically with the GPIO state change.
            let mut board = valves.lock().await;
            let mut opened = valve_opened_at.lock().await;
            board.set(zone_id, true);
            opened.insert(zone_id.to_string(), Instant::now());
            drop(opened);
            drop(board);

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
// Node status handling (MQTT Last Will / online announcements)
// ---------------------------------------------------------------------------

async fn handle_node_status(node_id: &str, payload: &[u8], shared: &RwLock<SystemState>) {
    let status = String::from_utf8_lossy(payload).trim().to_lowercase();
    let online = match status.as_str() {
        "online" => true,
        "offline" => false,
        _ => {
            warn!(node = %node_id, status = %status, "unknown node status payload");
            return;
        }
    };

    if online {
        info!(node = %node_id, "node online");
    } else {
        warn!(node = %node_id, "node offline (LWT or graceful disconnect)");
    }

    let mut st = shared.write().await;
    st.record_node_status(node_id, online);
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
