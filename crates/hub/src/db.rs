//! SQLite persistence layer (via sqlx): zones, sensors, readings, watering
//! events, and daily safety counters.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Pool, QueryBuilder, Sqlite};
use std::str::FromStr;
use time::OffsetDateTime;

#[derive(Clone)]
pub struct Db {
    pool: Pool<Sqlite>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoneConfig {
    pub zone_id: String,
    pub name: String,

    pub min_moisture: f32,
    pub target_moisture: f32,

    pub pulse_sec: i64,
    pub soak_min: i64,

    pub max_open_sec_per_day: i64,
    pub max_pulses_per_day: i64,
    pub stale_timeout_min: i64,

    pub valve_gpio_pin: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensorConfig {
    pub sensor_id: String,
    pub node_id: String,
    pub zone_id: String,
    pub raw_dry: i64,
    pub raw_wet: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DailyCounters {
    pub day: String, // YYYY-MM-DD
    pub zone_id: String,
    pub open_sec: i64,
    pub pulses: i64,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct ReadingRow {
    pub ts: i64,
    pub sensor_id: String,
    pub raw: i64,
    pub moisture: f64,
}

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct WateringEventRow {
    pub ts_start: i64,
    pub ts_end: i64,
    pub zone_id: String,
    pub reason: String,
    pub result: String,
}

/// Convert a raw ADC reading to a 0.0..=1.0 moisture fraction using
/// the sensor's dry/wet calibration endpoints.  Result is clamped so
/// out-of-range readings don't produce nonsensical values.
pub fn compute_moisture(raw: i64, raw_dry: i64, raw_wet: i64) -> f32 {
    let range = raw_dry - raw_wet;
    if range == 0 {
        return 0.0; // degenerate calibration — avoid div-by-zero
    }
    let m = (raw_dry - raw) as f64 / range as f64;
    m.clamp(0.0, 1.0) as f32
}

/// Margin beyond calibration endpoints that indicates a likely sensor failure.
/// A disconnected ADS1115 input reads ~32767; a shorted input reads ~0.
const SENSOR_FAILURE_MARGIN: i64 = 3000;

/// Returns `true` if the raw ADC value is plausibly within calibration range.
/// Values far outside the dry/wet endpoints suggest a disconnected, shorted,
/// or otherwise failed sensor.
pub fn is_reading_plausible(raw: i64, raw_dry: i64, raw_wet: i64) -> bool {
    let (lo, hi) = if raw_wet < raw_dry {
        (raw_wet, raw_dry)
    } else {
        (raw_dry, raw_wet)
    };
    raw >= lo - SENSOR_FAILURE_MARGIN && raw <= hi + SENSOR_FAILURE_MARGIN
}

impl Db {
    /// db_url examples:
    /// - "sqlite:/home/pi/irrigation/irrigation.db"
    /// - "sqlite::memory:" (tests)
    pub async fn connect(db_url: &str) -> Result<Self> {
        let options = SqliteConnectOptions::from_str(db_url)
            .with_context(|| format!("invalid sqlite connection string: {db_url}"))?
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true);

        let pool = SqlitePoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .with_context(|| format!("failed to connect to sqlite db: {db_url}"))?;

        Ok(Self { pool })
    }

    #[allow(dead_code)]
    pub fn pool(&self) -> &Pool<Sqlite> {
        &self.pool
    }

    /// Runs SQLx migrations from ./migrations.
    pub async fn migrate(&self) -> Result<()> {
        sqlx::migrate!("./migrations")
            .run(&self.pool)
            .await
            .context("failed to run migrations")?;
        Ok(())
    }

    // ----------------------------
    // Zone config
    // ----------------------------

    pub async fn upsert_zone(&self, z: &ZoneConfig) -> Result<()> {
        let min_m = z.min_moisture as f64;
        let target_m = z.target_moisture as f64;
        sqlx::query!(
            r#"
            INSERT INTO zones (
              zone_id, name,
              min_moisture, target_moisture,
              pulse_sec, soak_min,
              max_open_sec_per_day, max_pulses_per_day, stale_timeout_min,
              valve_gpio_pin
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(zone_id) DO UPDATE SET
              name=excluded.name,
              min_moisture=excluded.min_moisture,
              target_moisture=excluded.target_moisture,
              pulse_sec=excluded.pulse_sec,
              soak_min=excluded.soak_min,
              max_open_sec_per_day=excluded.max_open_sec_per_day,
              max_pulses_per_day=excluded.max_pulses_per_day,
              stale_timeout_min=excluded.stale_timeout_min,
              valve_gpio_pin=excluded.valve_gpio_pin
            "#,
            z.zone_id,
            z.name,
            min_m,
            target_m,
            z.pulse_sec,
            z.soak_min,
            z.max_open_sec_per_day,
            z.max_pulses_per_day,
            z.stale_timeout_min,
            z.valve_gpio_pin
        )
        .execute(&self.pool)
        .await
        .context("upsert_zone failed")?;
        Ok(())
    }

    pub async fn load_zones(&self) -> Result<Vec<ZoneConfig>> {
        let rows = sqlx::query!(
            r#"
            SELECT zone_id as "zone_id!", name,
                   min_moisture, target_moisture,
                   pulse_sec, soak_min,
                   max_open_sec_per_day, max_pulses_per_day, stale_timeout_min,
                   valve_gpio_pin
            FROM zones
            ORDER BY zone_id
            "#
        )
        .fetch_all(&self.pool)
        .await
        .context("load_zones failed")?;

        Ok(rows
            .into_iter()
            .map(|r| ZoneConfig {
                zone_id: r.zone_id,
                name: r.name,
                min_moisture: r.min_moisture as f32,
                target_moisture: r.target_moisture as f32,
                pulse_sec: r.pulse_sec,
                soak_min: r.soak_min,
                max_open_sec_per_day: r.max_open_sec_per_day,
                max_pulses_per_day: r.max_pulses_per_day,
                stale_timeout_min: r.stale_timeout_min,
                valve_gpio_pin: r.valve_gpio_pin,
            })
            .collect())
    }

    pub async fn get_zone(&self, zone_id: &str) -> Result<Option<ZoneConfig>> {
        let r = sqlx::query!(
            r#"
            SELECT zone_id as "zone_id!", name,
                   min_moisture, target_moisture,
                   pulse_sec, soak_min,
                   max_open_sec_per_day, max_pulses_per_day, stale_timeout_min,
                   valve_gpio_pin
            FROM zones
            WHERE zone_id = ?
            "#,
            zone_id
        )
        .fetch_optional(&self.pool)
        .await
        .context("get_zone failed")?;

        Ok(r.map(|r| ZoneConfig {
            zone_id: r.zone_id,
            name: r.name,
            min_moisture: r.min_moisture as f32,
            target_moisture: r.target_moisture as f32,
            pulse_sec: r.pulse_sec,
            soak_min: r.soak_min,
            max_open_sec_per_day: r.max_open_sec_per_day,
            max_pulses_per_day: r.max_pulses_per_day,
            stale_timeout_min: r.stale_timeout_min,
            valve_gpio_pin: r.valve_gpio_pin,
        }))
    }

    pub async fn delete_zone(&self, zone_id: &str) -> Result<bool> {
        let result = sqlx::query!("DELETE FROM zones WHERE zone_id = ?", zone_id)
            .execute(&self.pool)
            .await
            .context("delete_zone failed")?;
        Ok(result.rows_affected() > 0)
    }

    // ----------------------------
    // Sensor config
    // ----------------------------

    pub async fn upsert_sensor(&self, s: &SensorConfig) -> Result<()> {
        sqlx::query!(
            r#"
            INSERT INTO sensors (sensor_id, node_id, zone_id, raw_dry, raw_wet)
            VALUES (?, ?, ?, ?, ?)
            ON CONFLICT(sensor_id) DO UPDATE SET
              node_id=excluded.node_id,
              zone_id=excluded.zone_id,
              raw_dry=excluded.raw_dry,
              raw_wet=excluded.raw_wet
            "#,
            s.sensor_id,
            s.node_id,
            s.zone_id,
            s.raw_dry,
            s.raw_wet
        )
        .execute(&self.pool)
        .await
        .context("upsert_sensor failed")?;
        Ok(())
    }

    pub async fn load_sensors(&self) -> Result<Vec<SensorConfig>> {
        let rows = sqlx::query!(
            r#"
            SELECT sensor_id as "sensor_id!", node_id, zone_id, raw_dry, raw_wet
            FROM sensors
            ORDER BY sensor_id
            "#
        )
        .fetch_all(&self.pool)
        .await
        .context("load_sensors failed")?;

        Ok(rows
            .into_iter()
            .map(|r| SensorConfig {
                sensor_id: r.sensor_id,
                node_id: r.node_id,
                zone_id: r.zone_id,
                raw_dry: r.raw_dry,
                raw_wet: r.raw_wet,
            })
            .collect())
    }

    #[allow(dead_code)] // reserved for future per-node diagnostics
    pub async fn sensors_for_node(&self, node_id: &str) -> Result<Vec<SensorConfig>> {
        let rows = sqlx::query!(
            r#"
            SELECT sensor_id as "sensor_id!", node_id, zone_id, raw_dry, raw_wet
            FROM sensors
            WHERE node_id = ?
            ORDER BY sensor_id
            "#,
            node_id
        )
        .fetch_all(&self.pool)
        .await
        .context("sensors_for_node failed")?;

        Ok(rows
            .into_iter()
            .map(|r| SensorConfig {
                sensor_id: r.sensor_id,
                node_id: r.node_id,
                zone_id: r.zone_id,
                raw_dry: r.raw_dry,
                raw_wet: r.raw_wet,
            })
            .collect())
    }

    pub async fn get_sensor(&self, sensor_id: &str) -> Result<Option<SensorConfig>> {
        let r = sqlx::query!(
            r#"
            SELECT sensor_id as "sensor_id!", node_id, zone_id, raw_dry, raw_wet
            FROM sensors
            WHERE sensor_id = ?
            "#,
            sensor_id
        )
        .fetch_optional(&self.pool)
        .await
        .context("get_sensor failed")?;

        Ok(r.map(|r| SensorConfig {
            sensor_id: r.sensor_id,
            node_id: r.node_id,
            zone_id: r.zone_id,
            raw_dry: r.raw_dry,
            raw_wet: r.raw_wet,
        }))
    }

    pub async fn delete_sensor(&self, sensor_id: &str) -> Result<bool> {
        let result = sqlx::query!("DELETE FROM sensors WHERE sensor_id = ?", sensor_id)
            .execute(&self.pool)
            .await
            .context("delete_sensor failed")?;
        Ok(result.rows_affected() > 0)
    }

    // ----------------------------
    // Readings + aggregation helpers
    // ----------------------------

    pub async fn insert_reading(
        &self,
        ts: i64,
        sensor_id: &str,
        raw: i64,
        moisture: f32,
    ) -> Result<()> {
        let moisture_f64 = moisture as f64;
        sqlx::query!(
            r#"
            INSERT INTO readings (ts, sensor_id, raw, moisture)
            VALUES (?, ?, ?, ?)
            "#,
            ts,
            sensor_id,
            raw,
            moisture_f64
        )
        .execute(&self.pool)
        .await
        .context("insert_reading failed")?;
        Ok(())
    }

    /// Returns the newest moisture reading for a given zone across its sensors.
    /// (V1 simple approach: max(ts) across zone’s sensors)
    pub async fn latest_zone_moisture(&self, zone_id: &str) -> Result<Option<(i64, f32)>> {
        let row = sqlx::query!(
            r#"
            SELECT r.ts as ts, r.moisture as moisture
            FROM readings r
            JOIN sensors s ON s.sensor_id = r.sensor_id
            WHERE s.zone_id = ?
            ORDER BY r.ts DESC
            LIMIT 1
            "#,
            zone_id
        )
        .fetch_optional(&self.pool)
        .await
        .context("latest_zone_moisture failed")?;

        Ok(row.map(|r| (r.ts, r.moisture as f32)))
    }

    /// Returns a (simple) average moisture over the last N readings for a zone.
    pub async fn avg_zone_moisture_last_n(&self, zone_id: &str, n: i64) -> Result<Option<f32>> {
        let row = sqlx::query!(
            r#"
            SELECT AVG(r.moisture) as avg_m
            FROM (
              SELECT r.moisture
              FROM readings r
              JOIN sensors s ON s.sensor_id = r.sensor_id
              WHERE s.zone_id = ?
              ORDER BY r.ts DESC
              LIMIT ?
            ) r
            "#,
            zone_id,
            n
        )
        .fetch_one(&self.pool)
        .await
        .context("avg_zone_moisture_last_n failed")?;

        Ok(row.avg_m.map(|v| v as f32))
    }

    pub async fn list_readings(
        &self,
        sensor_id: Option<&str>,
        zone_id: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ReadingRow>> {
        let mut qb = QueryBuilder::<Sqlite>::new(
            "SELECT r.ts AS ts, r.sensor_id AS sensor_id, r.raw AS raw, r.moisture AS moisture FROM readings r",
        );

        if zone_id.is_some() {
            qb.push(" JOIN sensors s ON s.sensor_id = r.sensor_id");
        }

        let mut has_where = false;
        if let Some(sid) = sensor_id {
            qb.push(" WHERE r.sensor_id = ");
            qb.push_bind(sid.to_string());
            has_where = true;
        }
        if let Some(zid) = zone_id {
            qb.push(if has_where { " AND " } else { " WHERE " });
            qb.push("s.zone_id = ");
            qb.push_bind(zid.to_string());
        }

        qb.push(" ORDER BY r.ts DESC LIMIT ");
        qb.push_bind(limit);
        qb.push(" OFFSET ");
        qb.push_bind(offset);

        let rows = qb
            .build_query_as::<ReadingRow>()
            .fetch_all(&self.pool)
            .await
            .context("list_readings failed")?;

        Ok(rows)
    }

    /// Delete readings older than the given number of days and reclaim disk space.
    pub async fn prune_old_readings(&self, retention_days: i64) -> Result<u64> {
        let cutoff = OffsetDateTime::now_utc().unix_timestamp() - (retention_days * 86400);
        let result = sqlx::query!("DELETE FROM readings WHERE ts < ?", cutoff)
            .execute(&self.pool)
            .await
            .context("prune_old_readings failed")?;

        // Reclaim freed pages without locking the entire DB
        sqlx::query("PRAGMA incremental_vacuum(100)")
            .execute(&self.pool)
            .await
            .context("incremental_vacuum failed")?;

        Ok(result.rows_affected())
    }

    // ----------------------------
    // Watering events
    // ----------------------------

    pub async fn insert_watering_event(
        &self,
        ts_start: i64,
        ts_end: i64,
        zone_id: &str,
        reason: &str,
        result: &str,
    ) -> Result<()> {
        sqlx::query!(
            r#"
            INSERT INTO watering_events (ts_start, ts_end, zone_id, reason, result)
            VALUES (?, ?, ?, ?, ?)
            "#,
            ts_start,
            ts_end,
            zone_id,
            reason,
            result
        )
        .execute(&self.pool)
        .await
        .context("insert_watering_event failed")?;
        Ok(())
    }

    pub async fn list_watering_events(
        &self,
        zone_id: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<WateringEventRow>> {
        let mut qb = QueryBuilder::<Sqlite>::new(
            "SELECT ts_start, ts_end, zone_id, reason, result FROM watering_events",
        );

        if let Some(zid) = zone_id {
            qb.push(" WHERE zone_id = ");
            qb.push_bind(zid.to_string());
        }

        qb.push(" ORDER BY ts_start DESC LIMIT ");
        qb.push_bind(limit);
        qb.push(" OFFSET ");
        qb.push_bind(offset);

        let rows = qb
            .build_query_as::<WateringEventRow>()
            .fetch_all(&self.pool)
            .await
            .context("list_watering_events failed")?;

        Ok(rows)
    }

    // ----------------------------
    // Daily counters (safety limits)
    // ----------------------------

    pub fn today_yyyy_mm_dd() -> String {
        let now = OffsetDateTime::now_utc();
        format!(
            "{:04}-{:02}-{:02}",
            now.year(),
            now.month() as u8,
            now.day()
        )
    }

    pub async fn get_daily_counters(&self, day: &str, zone_id: &str) -> Result<DailyCounters> {
        let row = sqlx::query!(
            r#"
            SELECT day, zone_id, open_sec, pulses
            FROM zone_daily_counters
            WHERE day = ? AND zone_id = ?
            "#,
            day,
            zone_id
        )
        .fetch_optional(&self.pool)
        .await
        .context("get_daily_counters failed")?;

        Ok(match row {
            Some(r) => DailyCounters {
                day: r.day,
                zone_id: r.zone_id,
                open_sec: r.open_sec,
                pulses: r.pulses,
            },
            None => DailyCounters {
                day: day.to_string(),
                zone_id: zone_id.to_string(),
                open_sec: 0,
                pulses: 0,
            },
        })
    }

    pub async fn ensure_daily_row(&self, day: &str, zone_id: &str) -> Result<()> {
        sqlx::query!(
            r#"
            INSERT INTO zone_daily_counters (day, zone_id, open_sec, pulses)
            VALUES (?, ?, 0, 0)
            ON CONFLICT(day, zone_id) DO NOTHING
            "#,
            day,
            zone_id
        )
        .execute(&self.pool)
        .await
        .context("ensure_daily_row failed")?;
        Ok(())
    }

    pub async fn add_open_seconds(&self, day: &str, zone_id: &str, delta: i64) -> Result<()> {
        self.ensure_daily_row(day, zone_id).await?;
        sqlx::query!(
            r#"
            UPDATE zone_daily_counters
            SET open_sec = open_sec + ?
            WHERE day = ? AND zone_id = ?
            "#,
            delta,
            day,
            zone_id
        )
        .execute(&self.pool)
        .await
        .context("add_open_seconds failed")?;
        Ok(())
    }

    pub async fn add_pulse(&self, day: &str, zone_id: &str, delta: i64) -> Result<()> {
        self.ensure_daily_row(day, zone_id).await?;
        sqlx::query!(
            r#"
            UPDATE zone_daily_counters
            SET pulses = pulses + ?
            WHERE day = ? AND zone_id = ?
            "#,
            delta,
            day,
            zone_id
        )
        .execute(&self.pool)
        .await
        .context("add_pulse failed")?;
        Ok(())
    }

    /// Quick connectivity check — runs a trivial query.
    pub async fn health_check(&self) -> Result<()> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .context("db health check failed")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- compute_moisture -----------------------------------------------

    #[test]
    fn compute_moisture_mid_range() {
        let m = compute_moisture(19000, 26000, 12000);
        assert!((m - 0.5).abs() < 0.01);
    }

    #[test]
    fn compute_moisture_bone_dry() {
        let m = compute_moisture(26000, 26000, 12000);
        assert!((m - 0.0).abs() < 0.01);
    }

    #[test]
    fn compute_moisture_saturated() {
        let m = compute_moisture(12000, 26000, 12000);
        assert!((m - 1.0).abs() < 0.01);
    }

    #[test]
    fn compute_moisture_clamped_below() {
        let m = compute_moisture(30000, 26000, 12000);
        assert_eq!(m, 0.0);
    }

    #[test]
    fn compute_moisture_clamped_above() {
        let m = compute_moisture(5000, 26000, 12000);
        assert_eq!(m, 1.0);
    }

    #[test]
    fn compute_moisture_degenerate_calibration() {
        let m = compute_moisture(20000, 15000, 15000);
        assert_eq!(m, 0.0);
    }

    // -- is_reading_plausible -------------------------------------------

    #[test]
    fn plausible_reading_in_range() {
        assert!(is_reading_plausible(20000, 26000, 12000));
    }

    #[test]
    fn plausible_reading_at_dry() {
        assert!(is_reading_plausible(26000, 26000, 12000));
    }

    #[test]
    fn plausible_reading_at_wet() {
        assert!(is_reading_plausible(12000, 26000, 12000));
    }

    #[test]
    fn plausible_reading_slightly_beyond_range() {
        // Within the margin — still plausible
        assert!(is_reading_plausible(28000, 26000, 12000));
        assert!(is_reading_plausible(10000, 26000, 12000));
    }

    #[test]
    fn implausible_reading_disconnected_sensor() {
        // ADS1115 open input reads ~32767
        assert!(!is_reading_plausible(32767, 26000, 12000));
    }

    #[test]
    fn implausible_reading_shorted_sensor() {
        // Shorted to ground reads ~0
        assert!(!is_reading_plausible(0, 26000, 12000));
    }

    #[test]
    fn plausible_with_inverted_calibration() {
        // Some sensors have raw_wet > raw_dry
        assert!(is_reading_plausible(20000, 12000, 26000));
        assert!(!is_reading_plausible(32767, 12000, 26000));
    }

    // -- prune_old_readings ---------------------------------------------

    #[tokio::test]
    async fn prune_old_readings_removes_old_data() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        db.migrate().await.unwrap();

        // Insert a zone and sensor for FK constraints
        db.upsert_zone(&ZoneConfig {
            zone_id: "z1".into(),
            name: "Test".into(),
            min_moisture: 0.3,
            target_moisture: 0.5,
            pulse_sec: 30,
            soak_min: 20,
            max_open_sec_per_day: 180,
            max_pulses_per_day: 6,
            stale_timeout_min: 30,
            valve_gpio_pin: 17,
        })
        .await
        .unwrap();
        db.upsert_sensor(&SensorConfig {
            sensor_id: "s1".into(),
            node_id: "n1".into(),
            zone_id: "z1".into(),
            raw_dry: 26000,
            raw_wet: 12000,
        })
        .await
        .unwrap();

        // Insert an old reading (200 days ago) and a recent one
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let old_ts = now - (200 * 86400);
        db.insert_reading(old_ts, "s1", 20000, 0.5).await.unwrap();
        db.insert_reading(now, "s1", 20000, 0.5).await.unwrap();

        // Prune readings older than 90 days
        let deleted = db.prune_old_readings(90).await.unwrap();
        assert_eq!(deleted, 1);

        // Only the recent reading should remain
        let remaining = db.list_readings(None, None, 100, 0).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].ts, now);
    }

    // -- health_check ---------------------------------------------------

    #[tokio::test]
    async fn health_check_succeeds() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        db.health_check().await.unwrap();
    }
}
