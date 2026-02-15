//! Auto-watering scheduler: monitors zone moisture and triggers pulse/soak
//! watering cycles by publishing valve commands through MQTT.
//!
//! The scheduler is a pure decision engine — it publishes `ON`/`OFF` to
//! `valve/<zone_id>/set`, which round-trips through the broker back into
//! `handle_valve_command`.  All safety checks (daily limits, watchdog,
//! counters, watering-event logging, UI state updates) are handled by that
//! existing path; nothing is duplicated here.
//!
//! ## Per-zone state machine
//!
//! ```text
//! Idle ──[moisture < min]──▶ Watering ──[pulse_sec elapsed]──▶ Soaking
//!  ▲                                                              │
//!  └──────[moisture >= target]──────────────────────────────────────┘
//!  ▲                                                              │
//!  └────────────────────[moisture < target]── (another pulse) ────┘
//! ```

use std::collections::HashMap;
use std::time::Duration;

use rumqttc::{AsyncClient, QoS};
use tokio::time::Instant;
use tracing::{error, info, warn};

use crate::config::OperationMode;
use crate::db::{Db, ZoneConfig};
use crate::state::SharedState;

/// How often the scheduler evaluates each zone.
const TICK_INTERVAL_SEC: u64 = 30;

/// Number of recent readings to average when deciding moisture level.
const AVG_WINDOW: i64 = 5;

// ---------------------------------------------------------------------------
// Per-zone schedule state
// ---------------------------------------------------------------------------

enum ZoneScheduleState {
    /// Waiting for moisture to drop below `min_moisture`.
    Idle,
    /// Valve ON; waiting for `pulse_sec` to elapse before sending OFF.
    Watering { since: Instant },
    /// Valve OFF; waiting for `soak_min` to elapse before re-evaluating.
    Soaking { until: Instant },
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run the scheduler loop.  Intended to be `tokio::spawn`-ed from main.
pub async fn run(
    db: Db,
    zone_configs: HashMap<String, ZoneConfig>,
    mqtt: AsyncClient,
    shared: SharedState,
    max_concurrent_valves: usize,
    mode: OperationMode,
) {
    let mut states: HashMap<String, ZoneScheduleState> = zone_configs
        .keys()
        .map(|z| (z.clone(), ZoneScheduleState::Idle))
        .collect();

    // Brief startup delay so the first telemetry readings can arrive before
    // the scheduler starts making decisions on empty data.
    tokio::time::sleep(Duration::from_secs(TICK_INTERVAL_SEC)).await;

    let mut ticker = tokio::time::interval(Duration::from_secs(TICK_INTERVAL_SEC));

    info!(
        zones = zone_configs.len(),
        tick_sec = TICK_INTERVAL_SEC,
        max_concurrent_valves,
        ?mode,
        "scheduler started"
    );
    {
        let mut st = shared.write().await;
        st.record_scheduler(format!(
            "scheduler started (mode: {mode:?}, max concurrent valves: {max_concurrent_valves})"
        ));
    }

    loop {
        ticker.tick().await;

        // Snapshot how many valves are already open from SharedState, then
        // track any additional ones started in *this* tick.  MQTT round-trips
        // take ~ms to update SharedState, so without the local counter two
        // Idle zones evaluated in the same tick could both publish ON.
        let base_active = {
            let st = shared.read().await;
            st.zones.values().filter(|z| z.on).count()
        };
        let mut started_this_tick: usize = 0;

        for (zone_id, zone_cfg) in &zone_configs {
            let zone_state = states.get_mut(zone_id).expect("state map in sync");

            match zone_state {
                ZoneScheduleState::Idle => {
                    if mode == OperationMode::Auto
                        && base_active + started_this_tick >= max_concurrent_valves
                    {
                        continue;
                    }
                    handle_idle(
                        zone_id,
                        zone_cfg,
                        zone_state,
                        &db,
                        &mqtt,
                        &shared,
                        max_concurrent_valves,
                        mode,
                    )
                    .await;
                    if mode == OperationMode::Auto
                        && matches!(zone_state, ZoneScheduleState::Watering { .. })
                    {
                        started_this_tick += 1;
                    }
                }
                ZoneScheduleState::Watering { since } => {
                    handle_watering(zone_id, zone_cfg, *since, zone_state, &mqtt, &shared).await;
                }
                ZoneScheduleState::Soaking { until } => {
                    handle_soaking(zone_id, zone_cfg, *until, zone_state, &db, &shared).await;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// State handlers
// ---------------------------------------------------------------------------

/// Idle: check moisture and decide whether to start a watering pulse.
async fn handle_idle(
    zone_id: &str,
    cfg: &ZoneConfig,
    state: &mut ZoneScheduleState,
    db: &Db,
    mqtt: &AsyncClient,
    shared: &SharedState,
    max_concurrent_valves: usize,
    mode: OperationMode,
) {
    // ── Guards (auto mode only) ──────────────────────────────────
    if mode == OperationMode::Auto {
        let st = shared.read().await;
        if !st.mqtt_connected {
            return;
        }
        if let Some(z) = st.zones.get(zone_id) {
            if z.on {
                return;
            }
        }
        let active = st.zones.values().filter(|z| z.on).count();
        if active >= max_concurrent_valves {
            return;
        }
    }

    // ── Guard: fresh sensor data (both modes) ────────────────────
    let latest = match db.latest_zone_moisture(zone_id).await {
        Ok(Some(v)) => v,
        Ok(None) => return,
        Err(e) => {
            error!(zone = %zone_id, "scheduler: latest_zone_moisture failed: {e}");
            return;
        }
    };

    let now_ts = now_unix();
    let stale_secs = cfg.stale_timeout_min * 60;
    if now_ts - latest.0 > stale_secs {
        warn!(
            zone = %zone_id,
            age_sec = now_ts - latest.0,
            stale_timeout_sec = stale_secs,
            "scheduler: stale sensor data — skipping"
        );
        return;
    }

    // ── Guard: daily limits (auto mode only) ─────────────────────
    if mode == OperationMode::Auto {
        let today = Db::today_yyyy_mm_dd();
        match db.get_daily_counters(&today, zone_id).await {
            Ok(c) => {
                if c.pulses >= cfg.max_pulses_per_day || c.open_sec >= cfg.max_open_sec_per_day {
                    return;
                }
            }
            Err(e) => {
                error!(zone = %zone_id, "scheduler: get_daily_counters failed: {e}");
                return;
            }
        }
    }

    // ── Moisture check (both modes) ──────────────────────────────
    let avg_moisture = match db.avg_zone_moisture_last_n(zone_id, AVG_WINDOW).await {
        Ok(Some(v)) => v,
        Ok(None) => return,
        Err(e) => {
            error!(zone = %zone_id, "scheduler: avg_zone_moisture failed: {e}");
            return;
        }
    };

    if avg_moisture >= cfg.min_moisture {
        return;
    }

    // ── Monitor mode: record alert, stay idle ────────────────────
    if mode == OperationMode::Monitor {
        info!(
            zone = %zone_id,
            avg_moisture = format!("{avg_moisture:.3}"),
            min = format!("{:.3}", cfg.min_moisture),
            "scheduler: low moisture alert (monitor mode)"
        );
        {
            let mut st = shared.write().await;
            st.record_scheduler(format!(
                "{zone_id}: low moisture alert ({avg_moisture:.3} < min {:.3})",
                cfg.min_moisture
            ));
        }
        return; // stay Idle — no valve actuation
    }

    // ── Auto mode: trigger watering pulse ────────────────────────
    info!(
        zone = %zone_id,
        avg_moisture = format!("{avg_moisture:.3}"),
        min = format!("{:.3}", cfg.min_moisture),
        pulse_sec = cfg.pulse_sec,
        "scheduler: moisture below min — starting pulse"
    );

    if let Err(e) = mqtt
        .publish(
            format!("valve/{zone_id}/set"),
            QoS::AtLeastOnce,
            false,
            b"ON".to_vec(),
        )
        .await
    {
        error!(zone = %zone_id, "scheduler: failed to publish ON: {e}");
        return;
    }

    {
        let mut st = shared.write().await;
        st.record_scheduler(format!(
            "{zone_id}: pulse started (moisture {avg_moisture:.3} < min {:.3})",
            cfg.min_moisture
        ));
    }

    *state = ZoneScheduleState::Watering {
        since: Instant::now(),
    };
}

/// Watering: check if pulse duration has elapsed, then send OFF.
async fn handle_watering(
    zone_id: &str,
    cfg: &ZoneConfig,
    since: Instant,
    state: &mut ZoneScheduleState,
    mqtt: &AsyncClient,
    shared: &SharedState,
) {
    if since.elapsed().as_secs() < cfg.pulse_sec as u64 {
        return; // pulse still running
    }

    // Pulse complete — turn valve off and enter soak.
    if let Err(e) = mqtt
        .publish(
            format!("valve/{zone_id}/set"),
            QoS::AtLeastOnce,
            false,
            b"OFF".to_vec(),
        )
        .await
    {
        error!(zone = %zone_id, "scheduler: failed to publish OFF: {e}");
        // Don't transition — watchdog will catch it if OFF never arrives.
        return;
    }

    let soak_duration = Duration::from_secs(cfg.soak_min as u64 * 60);

    info!(
        zone = %zone_id,
        soak_min = cfg.soak_min,
        "scheduler: pulse complete — entering soak"
    );

    {
        let mut st = shared.write().await;
        st.record_scheduler(format!(
            "{zone_id}: pulse done, soaking {}min",
            cfg.soak_min
        ));
    }

    *state = ZoneScheduleState::Soaking {
        until: Instant::now() + soak_duration,
    };
}

/// Soaking: wait for soak timer, then re-check moisture.
async fn handle_soaking(
    zone_id: &str,
    cfg: &ZoneConfig,
    until: Instant,
    state: &mut ZoneScheduleState,
    db: &Db,
    shared: &SharedState,
) {
    if Instant::now() < until {
        return; // still soaking
    }

    // Soak complete — re-evaluate moisture.
    let avg_moisture = match db.avg_zone_moisture_last_n(zone_id, AVG_WINDOW).await {
        Ok(Some(v)) => v,
        Ok(None) => {
            // Lost all readings during soak — go idle to be safe.
            *state = ZoneScheduleState::Idle;
            return;
        }
        Err(e) => {
            error!(zone = %zone_id, "scheduler: avg_zone_moisture failed: {e}");
            *state = ZoneScheduleState::Idle;
            return;
        }
    };

    if avg_moisture >= cfg.target_moisture {
        info!(
            zone = %zone_id,
            avg_moisture = format!("{avg_moisture:.3}"),
            target = format!("{:.3}", cfg.target_moisture),
            "scheduler: target reached — returning to idle"
        );
        {
            let mut st = shared.write().await;
            st.record_scheduler(format!(
                "{zone_id}: target reached (moisture {avg_moisture:.3} >= target {:.3})",
                cfg.target_moisture
            ));
        }
        *state = ZoneScheduleState::Idle;
    } else {
        info!(
            zone = %zone_id,
            avg_moisture = format!("{avg_moisture:.3}"),
            target = format!("{:.3}", cfg.target_moisture),
            "scheduler: soak complete, moisture still below target — will re-evaluate"
        );
        {
            let mut st = shared.write().await;
            st.record_scheduler(format!(
                "{zone_id}: soak done, moisture {avg_moisture:.3} < target {:.3}",
                cfg.target_moisture
            ));
        }
        // Return to Idle so the full guard-check sequence runs again
        // (staleness, daily limits, MQTT connectivity) before the next pulse.
        *state = ZoneScheduleState::Idle;
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OperationMode;
    use crate::db::{Db, SensorConfig, ZoneConfig};
    use crate::state::SystemState;
    use std::sync::Arc;
    use tokio::sync::RwLock;

    /// Build a zone config with sensible defaults for testing.
    fn test_zone_cfg() -> ZoneConfig {
        ZoneConfig {
            zone_id: "z1".into(),
            name: "Test Zone".into(),
            min_moisture: 0.3,
            target_moisture: 0.5,
            pulse_sec: 30,
            soak_min: 20,
            max_open_sec_per_day: 180,
            max_pulses_per_day: 6,
            stale_timeout_min: 30,
            valve_gpio_pin: 17,
        }
    }

    /// Build a SharedState with one zone.
    fn test_shared() -> SharedState {
        Arc::new(RwLock::new(SystemState::new(&[("z1".to_string(), 17)], "auto")))
    }

    /// Set up an in-memory DB with a zone and sensor, then seed readings.
    async fn seeded_db(moisture_values: &[f32]) -> Db {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        db.migrate().await.unwrap();
        db.upsert_zone(&test_zone_cfg()).await.unwrap();
        db.upsert_sensor(&SensorConfig {
            sensor_id: "s1".into(),
            node_id: "n1".into(),
            zone_id: "z1".into(),
            raw_dry: 26000,
            raw_wet: 12000,
        })
        .await
        .unwrap();

        let now = now_unix();
        for (i, &m) in moisture_values.iter().enumerate() {
            // Space readings 10 seconds apart, most recent last.
            let ts = now - ((moisture_values.len() - 1 - i) as i64 * 10);
            // Reverse-engineer a raw value from moisture for the calibration
            // range: raw = raw_dry - moisture * (raw_dry - raw_wet)
            let raw = 26000 - (m * 14000.0) as i64;
            db.insert_reading(ts, "s1", raw, m).await.unwrap();
        }

        db
    }

    /// Create a minimal MQTT AsyncClient.  We never poll its event loop, so
    /// publishes just accumulate in the internal buffer — sufficient for
    /// verifying that handler logic transitions state correctly.
    ///
    /// Returns both the client and the event loop; the event loop must stay
    /// alive for the duration of the test so the internal channel remains open.
    fn test_mqtt() -> (AsyncClient, rumqttc::EventLoop) {
        let opts = rumqttc::MqttOptions::new("test-sched", "127.0.0.1", 1883);
        AsyncClient::new(opts, 10)
    }

    // -- Idle: no readings → stays idle ----------------------------------

    #[tokio::test]
    async fn idle_no_readings_stays_idle() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        db.migrate().await.unwrap();
        db.upsert_zone(&test_zone_cfg()).await.unwrap();

        let (mqtt, _el) = test_mqtt();
        let shared = test_shared();
        {
            let mut st = shared.write().await;
            st.mqtt_connected = true;
        }

        let mut state = ZoneScheduleState::Idle;
        handle_idle("z1", &test_zone_cfg(), &mut state, &db, &mqtt, &shared, 2, OperationMode::Auto).await;

        assert!(matches!(state, ZoneScheduleState::Idle));
    }

    // -- Idle: moisture above min → stays idle ---------------------------

    #[tokio::test]
    async fn idle_moisture_above_min_stays_idle() {
        let db = seeded_db(&[0.6, 0.6, 0.6, 0.6, 0.6]).await;
        let (mqtt, _el) = test_mqtt();
        let shared = test_shared();
        {
            let mut st = shared.write().await;
            st.mqtt_connected = true;
        }

        let mut state = ZoneScheduleState::Idle;
        handle_idle("z1", &test_zone_cfg(), &mut state, &db, &mqtt, &shared, 2, OperationMode::Auto).await;

        assert!(matches!(state, ZoneScheduleState::Idle));
    }

    // -- Idle: moisture below min → transitions to Watering ---------------

    #[tokio::test]
    async fn idle_moisture_below_min_starts_watering() {
        let db = seeded_db(&[0.2, 0.2, 0.2, 0.2, 0.2]).await;
        let (mqtt, _el) = test_mqtt();
        let shared = test_shared();
        {
            let mut st = shared.write().await;
            st.mqtt_connected = true;
        }

        let mut state = ZoneScheduleState::Idle;
        handle_idle("z1", &test_zone_cfg(), &mut state, &db, &mqtt, &shared, 2, OperationMode::Auto).await;

        assert!(matches!(state, ZoneScheduleState::Watering { .. }));
    }

    // -- Idle: MQTT disconnected → stays idle ----------------------------

    #[tokio::test]
    async fn idle_mqtt_disconnected_stays_idle() {
        let db = seeded_db(&[0.1, 0.1, 0.1, 0.1, 0.1]).await;
        let (mqtt, _el) = test_mqtt();
        let shared = test_shared();
        // mqtt_connected defaults to false

        let mut state = ZoneScheduleState::Idle;
        handle_idle("z1", &test_zone_cfg(), &mut state, &db, &mqtt, &shared, 2, OperationMode::Auto).await;

        assert!(matches!(state, ZoneScheduleState::Idle));
    }

    // -- Idle: zone already on → stays idle ------------------------------

    #[tokio::test]
    async fn idle_zone_already_on_stays_idle() {
        let db = seeded_db(&[0.1, 0.1, 0.1, 0.1, 0.1]).await;
        let (mqtt, _el) = test_mqtt();
        let shared = test_shared();
        {
            let mut st = shared.write().await;
            st.mqtt_connected = true;
            st.record_valve("z1", true);
        }

        let mut state = ZoneScheduleState::Idle;
        handle_idle("z1", &test_zone_cfg(), &mut state, &db, &mqtt, &shared, 2, OperationMode::Auto).await;

        assert!(matches!(state, ZoneScheduleState::Idle));
    }

    // -- Idle: daily limit exhausted → stays idle -------------------------

    #[tokio::test]
    async fn idle_daily_limit_exhausted_stays_idle() {
        let db = seeded_db(&[0.1, 0.1, 0.1, 0.1, 0.1]).await;
        let (mqtt, _el) = test_mqtt();
        let shared = test_shared();
        {
            let mut st = shared.write().await;
            st.mqtt_connected = true;
        }

        // Exhaust the pulse limit.
        let today = Db::today_yyyy_mm_dd();
        for _ in 0..6 {
            db.add_pulse(&today, "z1", 1).await.unwrap();
        }

        let mut state = ZoneScheduleState::Idle;
        handle_idle("z1", &test_zone_cfg(), &mut state, &db, &mqtt, &shared, 2, OperationMode::Auto).await;

        assert!(matches!(state, ZoneScheduleState::Idle));
    }

    // -- Watering: pulse not elapsed → stays Watering --------------------

    #[tokio::test]
    async fn watering_pulse_not_elapsed_stays_watering() {
        let (mqtt, _el) = test_mqtt();
        let shared = test_shared();

        let since = Instant::now(); // just started
        let mut state = ZoneScheduleState::Watering { since };
        handle_watering("z1", &test_zone_cfg(), since, &mut state, &mqtt, &shared).await;

        assert!(matches!(state, ZoneScheduleState::Watering { .. }));
    }

    // -- Watering: pulse elapsed → transitions to Soaking ----------------

    #[tokio::test]
    async fn watering_pulse_elapsed_transitions_to_soaking() {
        let (mqtt, _el) = test_mqtt();
        let shared = test_shared();

        // Simulate pulse_sec already elapsed.
        let since = Instant::now() - Duration::from_secs(31);
        let mut state = ZoneScheduleState::Watering { since };
        handle_watering("z1", &test_zone_cfg(), since, &mut state, &mqtt, &shared).await;

        assert!(matches!(state, ZoneScheduleState::Soaking { .. }));
    }

    // -- Soaking: not expired → stays Soaking ----------------------------

    #[tokio::test]
    async fn soaking_not_expired_stays_soaking() {
        let db = seeded_db(&[0.4, 0.4, 0.4, 0.4, 0.4]).await;
        let shared = test_shared();

        let until = Instant::now() + Duration::from_secs(600);
        let mut state = ZoneScheduleState::Soaking { until };
        handle_soaking("z1", &test_zone_cfg(), until, &mut state, &db, &shared).await;

        assert!(matches!(state, ZoneScheduleState::Soaking { .. }));
    }

    // -- Soaking: expired + moisture >= target → Idle --------------------

    #[tokio::test]
    async fn soaking_expired_target_reached_goes_idle() {
        let db = seeded_db(&[0.6, 0.6, 0.6, 0.6, 0.6]).await;
        let shared = test_shared();

        let until = Instant::now() - Duration::from_secs(1); // already expired
        let mut state = ZoneScheduleState::Soaking { until };
        handle_soaking("z1", &test_zone_cfg(), until, &mut state, &db, &shared).await;

        assert!(matches!(state, ZoneScheduleState::Idle));
    }

    // -- Soaking: expired + moisture < target → Idle (re-evaluate) -------

    #[tokio::test]
    async fn soaking_expired_below_target_goes_idle_for_reevaluation() {
        let db = seeded_db(&[0.35, 0.35, 0.35, 0.35, 0.35]).await;
        let shared = test_shared();

        let until = Instant::now() - Duration::from_secs(1);
        let mut state = ZoneScheduleState::Soaking { until };
        handle_soaking("z1", &test_zone_cfg(), until, &mut state, &db, &shared).await;

        // Returns to Idle so full guard checks run before next pulse.
        assert!(matches!(state, ZoneScheduleState::Idle));
    }

    // -- Soaking: expired + no readings → Idle (safe fallback) -----------

    #[tokio::test]
    async fn soaking_expired_no_readings_goes_idle() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        db.migrate().await.unwrap();
        db.upsert_zone(&test_zone_cfg()).await.unwrap();

        let shared = test_shared();

        let until = Instant::now() - Duration::from_secs(1);
        let mut state = ZoneScheduleState::Soaking { until };
        handle_soaking("z1", &test_zone_cfg(), until, &mut state, &db, &shared).await;

        assert!(matches!(state, ZoneScheduleState::Idle));
    }

    // -- Idle: stale data → stays idle -----------------------------------

    #[tokio::test]
    async fn idle_stale_data_stays_idle() {
        // Create a DB with readings that are old (>30 min stale_timeout_min).
        let db = Db::connect("sqlite::memory:").await.unwrap();
        db.migrate().await.unwrap();
        db.upsert_zone(&test_zone_cfg()).await.unwrap();
        db.upsert_sensor(&SensorConfig {
            sensor_id: "s1".into(),
            node_id: "n1".into(),
            zone_id: "z1".into(),
            raw_dry: 26000,
            raw_wet: 12000,
        })
        .await
        .unwrap();

        // Insert a reading from 2 hours ago.
        let old_ts = now_unix() - 7200;
        db.insert_reading(old_ts, "s1", 22000, 0.1).await.unwrap();

        let (mqtt, _el) = test_mqtt();
        let shared = test_shared();
        {
            let mut st = shared.write().await;
            st.mqtt_connected = true;
        }

        let mut state = ZoneScheduleState::Idle;
        handle_idle("z1", &test_zone_cfg(), &mut state, &db, &mqtt, &shared, 2, OperationMode::Auto).await;

        assert!(matches!(state, ZoneScheduleState::Idle));
    }

    // -- Idle: concurrent valve limit reached → stays idle ----------------

    #[tokio::test]
    async fn idle_concurrent_limit_reached_stays_idle() {
        let db = seeded_db(&[0.1, 0.1, 0.1, 0.1, 0.1]).await;
        let (mqtt, _el) = test_mqtt();

        // Two-zone shared state: z2 already has its valve ON.
        let shared: SharedState = Arc::new(RwLock::new(SystemState::new(&[
            ("z1".to_string(), 17),
            ("z2".to_string(), 27),
        ], "auto")));
        {
            let mut st = shared.write().await;
            st.mqtt_connected = true;
            st.record_valve("z2", true);
        }

        let mut state = ZoneScheduleState::Idle;
        // max_concurrent_valves = 1 → z2 fills the single slot.
        handle_idle("z1", &test_zone_cfg(), &mut state, &db, &mqtt, &shared, 1, OperationMode::Auto).await;

        assert!(matches!(state, ZoneScheduleState::Idle));
    }

    #[tokio::test]
    async fn idle_concurrent_limit_not_reached_starts_watering() {
        let db = seeded_db(&[0.1, 0.1, 0.1, 0.1, 0.1]).await;
        let (mqtt, _el) = test_mqtt();

        let shared: SharedState = Arc::new(RwLock::new(SystemState::new(&[
            ("z1".to_string(), 17),
            ("z2".to_string(), 27),
        ], "auto")));
        {
            let mut st = shared.write().await;
            st.mqtt_connected = true;
            st.record_valve("z2", true); // z2 is on
        }

        let mut state = ZoneScheduleState::Idle;
        // max_concurrent_valves = 2 → one slot still available.
        handle_idle("z1", &test_zone_cfg(), &mut state, &db, &mqtt, &shared, 2, OperationMode::Auto).await;

        assert!(matches!(state, ZoneScheduleState::Watering { .. }));
    }

    // -- Monitor mode: low moisture → stays idle, records alert --------

    #[tokio::test]
    async fn monitor_mode_low_moisture_stays_idle_records_alert() {
        let db = seeded_db(&[0.2, 0.2, 0.2, 0.2, 0.2]).await;
        let (mqtt, _el) = test_mqtt();
        let shared = test_shared();

        let mut state = ZoneScheduleState::Idle;
        handle_idle(
            "z1", &test_zone_cfg(), &mut state, &db, &mqtt, &shared, 2,
            OperationMode::Monitor,
        )
        .await;

        // Must stay Idle — never transitions to Watering.
        assert!(matches!(state, ZoneScheduleState::Idle));

        // Must have recorded a low-moisture alert event.
        let st = shared.read().await;
        let scheduler_events: Vec<_> = st
            .events
            .iter()
            .filter(|e| matches!(e.kind, crate::state::EventKind::Scheduler))
            .collect();
        assert!(
            !scheduler_events.is_empty(),
            "expected at least one scheduler event"
        );
        assert!(
            scheduler_events.last().unwrap().detail.contains("low moisture alert"),
            "expected low moisture alert, got: {}",
            scheduler_events.last().unwrap().detail
        );
    }

    // -- Monitor mode: adequate moisture → stays idle, no alert --------

    #[tokio::test]
    async fn monitor_mode_adequate_moisture_stays_idle_no_alert() {
        let db = seeded_db(&[0.6, 0.6, 0.6, 0.6, 0.6]).await;
        let (mqtt, _el) = test_mqtt();
        let shared = test_shared();

        let mut state = ZoneScheduleState::Idle;
        handle_idle(
            "z1", &test_zone_cfg(), &mut state, &db, &mqtt, &shared, 2,
            OperationMode::Monitor,
        )
        .await;

        assert!(matches!(state, ZoneScheduleState::Idle));

        // No scheduler events should be recorded.
        let st = shared.read().await;
        let scheduler_events: Vec<_> = st
            .events
            .iter()
            .filter(|e| matches!(e.kind, crate::state::EventKind::Scheduler))
            .collect();
        assert!(
            scheduler_events.is_empty(),
            "expected no scheduler events, got: {:?}",
            scheduler_events
        );
    }

    // -- Monitor mode: skips MQTT connectivity check --------------------

    #[tokio::test]
    async fn monitor_mode_ignores_mqtt_disconnected() {
        let db = seeded_db(&[0.2, 0.2, 0.2, 0.2, 0.2]).await;
        let (mqtt, _el) = test_mqtt();
        let shared = test_shared();
        // mqtt_connected defaults to false — in auto mode this would skip.

        let mut state = ZoneScheduleState::Idle;
        handle_idle(
            "z1", &test_zone_cfg(), &mut state, &db, &mqtt, &shared, 2,
            OperationMode::Monitor,
        )
        .await;

        // In monitor mode, the MQTT guard is skipped, so the alert IS recorded.
        let st = shared.read().await;
        let scheduler_events: Vec<_> = st
            .events
            .iter()
            .filter(|e| matches!(e.kind, crate::state::EventKind::Scheduler))
            .collect();
        assert!(
            !scheduler_events.is_empty(),
            "expected alert even with MQTT disconnected in monitor mode"
        );
    }
}
