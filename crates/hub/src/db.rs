use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::{Pool, Sqlite};
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
            .max_connections(5)
            .connect_with(options)
            .await
            .with_context(|| format!("failed to connect to sqlite db: {db_url}"))?;

        Ok(Self { pool })
    }

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

    // ----------------------------
    // Readings + aggregation helpers
    // ----------------------------

    pub async fn insert_reading(&self, ts: i64, sensor_id: &str, raw: i64, moisture: f32) -> Result<()> {
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

    // ----------------------------
    // Daily counters (safety limits)
    // ----------------------------

    pub fn today_yyyy_mm_dd() -> String {
        let now = OffsetDateTime::now_utc();
        format!("{:04}-{:02}-{:02}", now.year(), now.month() as u8, now.day())
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
}