//! TOML config file loading and database seeding for zones and sensors.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::db::{Db, SensorConfig, ZoneConfig};

// ---------------------------------------------------------------------------
// Config file structures
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub zones: Vec<ZoneEntry>,
    #[serde(default)]
    pub sensors: Vec<SensorEntry>,
}

#[derive(Debug, Deserialize)]
pub struct ZoneEntry {
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

#[derive(Debug, Deserialize)]
pub struct SensorEntry {
    pub sensor_id: String,
    pub node_id: String,
    pub zone_id: String,
    pub raw_dry: i64,
    pub raw_wet: i64,
}

// ---------------------------------------------------------------------------
// Load + apply
// ---------------------------------------------------------------------------

/// Read and parse a TOML config file.
pub fn load(path: &str) -> Result<Config> {
    let contents =
        std::fs::read_to_string(path).with_context(|| format!("failed to read config: {path}"))?;
    let config: Config =
        toml::from_str(&contents).with_context(|| format!("failed to parse config: {path}"))?;
    Ok(config)
}

/// Upsert all zones and sensors from the config into the database.
pub async fn apply(config: &Config, db: &Db) -> Result<()> {
    for z in &config.zones {
        db.upsert_zone(&ZoneConfig {
            zone_id: z.zone_id.clone(),
            name: z.name.clone(),
            min_moisture: z.min_moisture,
            target_moisture: z.target_moisture,
            pulse_sec: z.pulse_sec,
            soak_min: z.soak_min,
            max_open_sec_per_day: z.max_open_sec_per_day,
            max_pulses_per_day: z.max_pulses_per_day,
            stale_timeout_min: z.stale_timeout_min,
            valve_gpio_pin: z.valve_gpio_pin,
        })
        .await
        .with_context(|| format!("failed to upsert zone '{}'", z.zone_id))?;
    }

    for s in &config.sensors {
        db.upsert_sensor(&SensorConfig {
            sensor_id: s.sensor_id.clone(),
            node_id: s.node_id.clone(),
            zone_id: s.zone_id.clone(),
            raw_dry: s.raw_dry,
            raw_wet: s.raw_wet,
        })
        .await
        .with_context(|| format!("failed to upsert sensor '{}'", s.sensor_id))?;
    }

    eprintln!(
        "config applied — {} zone(s), {} sensor(s)",
        config.zones.len(),
        config.sensors.len()
    );

    Ok(())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_config() {
        let toml_str = r#"
[[zones]]
zone_id = "z1"
name = "Zone 1"
min_moisture = 0.3
target_moisture = 0.5
pulse_sec = 30
soak_min = 20
max_open_sec_per_day = 180
max_pulses_per_day = 6
stale_timeout_min = 30
valve_gpio_pin = 17

[[sensors]]
sensor_id = "node-a/s1"
node_id = "node-a"
zone_id = "z1"
raw_dry = 26000
raw_wet = 12000
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.zones.len(), 1);
        assert_eq!(config.sensors.len(), 1);
        assert_eq!(config.zones[0].zone_id, "z1");
        assert_eq!(config.sensors[0].sensor_id, "node-a/s1");
    }

    #[test]
    fn parse_empty_config() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.zones.is_empty());
        assert!(config.sensors.is_empty());
    }

    #[tokio::test]
    async fn apply_seeds_database() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        db.migrate().await.unwrap();

        let config: Config = toml::from_str(
            r#"
[[zones]]
zone_id = "z1"
name = "Test Zone"
min_moisture = 0.3
target_moisture = 0.5
pulse_sec = 30
soak_min = 20
max_open_sec_per_day = 180
max_pulses_per_day = 6
stale_timeout_min = 30
valve_gpio_pin = 17

[[sensors]]
sensor_id = "node-a/s1"
node_id = "node-a"
zone_id = "z1"
raw_dry = 26000
raw_wet = 12000
"#,
        )
        .unwrap();

        apply(&config, &db).await.unwrap();

        let zones = db.load_zones().await.unwrap();
        assert_eq!(zones.len(), 1);
        assert_eq!(zones[0].zone_id, "z1");

        let sensors = db.load_sensors().await.unwrap();
        assert_eq!(sensors.len(), 1);
        assert_eq!(sensors[0].sensor_id, "node-a/s1");
        assert_eq!(sensors[0].node_id, "node-a");
    }
}
