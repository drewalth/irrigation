//! TOML config file loading, validation, and database seeding for zones and
//! sensors.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashSet;

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
// GPIO whitelist
// ---------------------------------------------------------------------------

/// BCM GPIO pins available on the Raspberry Pi 40-pin header for general
/// use. GPIO 0-1 are reserved for the ID EEPROM and must never be used.
/// GPIO 28+ are not exposed on the standard header.
const VALID_GPIO_PINS: &[i64] = &[
    2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27,
];

/// Maximum single-ended reading from the ADS1115 (15-bit unsigned).
const ADS1115_MAX: i64 = 32767;

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

impl Config {
    /// Validate all config entries. Returns `Ok(())` or an error describing
    /// every violation found (not just the first one).
    pub fn validate(&self) -> Result<()> {
        let mut errors: Vec<String> = Vec::new();

        self.validate_zones(&mut errors);
        self.validate_sensors(&mut errors);

        if errors.is_empty() {
            Ok(())
        } else {
            bail!(
                "config validation failed ({} error{}):\n  - {}",
                errors.len(),
                if errors.len() == 1 { "" } else { "s" },
                errors.join("\n  - ")
            );
        }
    }

    fn validate_zones(&self, errors: &mut Vec<String>) {
        let mut seen_ids: HashSet<&str> = HashSet::new();
        let mut seen_pins: HashSet<i64> = HashSet::new();

        for (i, z) in self.zones.iter().enumerate() {
            let ctx = || {
                if z.zone_id.is_empty() {
                    format!("zones[{i}]")
                } else {
                    format!("zone '{}'", z.zone_id)
                }
            };

            // ── Identity ────────────────────────────────────────
            if z.zone_id.trim().is_empty() {
                errors.push(format!("{}: zone_id is empty", ctx()));
            } else if !seen_ids.insert(&z.zone_id) {
                errors.push(format!("{}: duplicate zone_id", ctx()));
            }

            if z.name.trim().is_empty() {
                errors.push(format!("{}: name is empty", ctx()));
            }

            // ── Moisture bounds ─────────────────────────────────
            if !(0.0..=1.0).contains(&z.min_moisture) {
                errors.push(format!(
                    "{}: min_moisture {} out of range [0.0, 1.0]",
                    ctx(),
                    z.min_moisture
                ));
            }
            if !(0.0..=1.0).contains(&z.target_moisture) {
                errors.push(format!(
                    "{}: target_moisture {} out of range [0.0, 1.0]",
                    ctx(),
                    z.target_moisture
                ));
            }
            if z.target_moisture <= z.min_moisture {
                errors.push(format!(
                    "{}: target_moisture ({}) must be greater than min_moisture ({})",
                    ctx(),
                    z.target_moisture,
                    z.min_moisture
                ));
            }

            // ── Timing values (all must be positive) ────────────
            if z.pulse_sec <= 0 {
                errors.push(format!(
                    "{}: pulse_sec must be positive, got {}",
                    ctx(),
                    z.pulse_sec
                ));
            }
            if z.soak_min <= 0 {
                errors.push(format!(
                    "{}: soak_min must be positive, got {}",
                    ctx(),
                    z.soak_min
                ));
            }
            if z.max_open_sec_per_day <= 0 {
                errors.push(format!(
                    "{}: max_open_sec_per_day must be positive, got {}",
                    ctx(),
                    z.max_open_sec_per_day
                ));
            }
            if z.max_pulses_per_day <= 0 {
                errors.push(format!(
                    "{}: max_pulses_per_day must be positive, got {}",
                    ctx(),
                    z.max_pulses_per_day
                ));
            }
            if z.stale_timeout_min <= 0 {
                errors.push(format!(
                    "{}: stale_timeout_min must be positive, got {}",
                    ctx(),
                    z.stale_timeout_min
                ));
            }

            // pulse_sec cannot exceed the daily maximum
            if z.pulse_sec > 0 && z.max_open_sec_per_day > 0 && z.pulse_sec > z.max_open_sec_per_day
            {
                errors.push(format!(
                    "{}: pulse_sec ({}) exceeds max_open_sec_per_day ({})",
                    ctx(),
                    z.pulse_sec,
                    z.max_open_sec_per_day
                ));
            }

            // ── GPIO pin whitelist ──────────────────────────────
            if !VALID_GPIO_PINS.contains(&z.valve_gpio_pin) {
                errors.push(format!(
                    "{}: valve_gpio_pin {} is not a valid BCM GPIO pin (allowed: 2-27)",
                    ctx(),
                    z.valve_gpio_pin
                ));
            } else if !seen_pins.insert(z.valve_gpio_pin) {
                errors.push(format!(
                    "{}: valve_gpio_pin {} is already used by another zone",
                    ctx(),
                    z.valve_gpio_pin
                ));
            }
        }
    }

    fn validate_sensors(&self, errors: &mut Vec<String>) {
        let zone_ids: HashSet<&str> = self.zones.iter().map(|z| z.zone_id.as_str()).collect();
        let mut seen_ids: HashSet<&str> = HashSet::new();

        for (i, s) in self.sensors.iter().enumerate() {
            let ctx = || {
                if s.sensor_id.is_empty() {
                    format!("sensors[{i}]")
                } else {
                    format!("sensor '{}'", s.sensor_id)
                }
            };

            // ── Identity ────────────────────────────────────────
            if s.sensor_id.trim().is_empty() {
                errors.push(format!("{}: sensor_id is empty", ctx()));
            } else if !seen_ids.insert(&s.sensor_id) {
                errors.push(format!("{}: duplicate sensor_id", ctx()));
            }

            if s.node_id.trim().is_empty() {
                errors.push(format!("{}: node_id is empty", ctx()));
            }

            if s.zone_id.trim().is_empty() {
                errors.push(format!("{}: zone_id is empty", ctx()));
            } else if !zone_ids.contains(s.zone_id.as_str()) {
                errors.push(format!(
                    "{}: zone_id '{}' does not match any defined zone",
                    ctx(),
                    s.zone_id
                ));
            }

            // ── ADC calibration bounds ──────────────────────────
            if s.raw_dry < 0 || s.raw_dry > ADS1115_MAX {
                errors.push(format!(
                    "{}: raw_dry {} out of ADS1115 range [0, {ADS1115_MAX}]",
                    ctx(),
                    s.raw_dry
                ));
            }
            if s.raw_wet < 0 || s.raw_wet > ADS1115_MAX {
                errors.push(format!(
                    "{}: raw_wet {} out of ADS1115 range [0, {ADS1115_MAX}]",
                    ctx(),
                    s.raw_wet
                ));
            }
            if s.raw_dry == s.raw_wet {
                errors.push(format!(
                    "{}: raw_dry and raw_wet are both {} — calibration range is zero",
                    ctx(),
                    s.raw_dry
                ));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Load + apply
// ---------------------------------------------------------------------------

/// Read, parse, and validate a TOML config file.
pub fn load(path: &str) -> Result<Config> {
    let contents =
        std::fs::read_to_string(path).with_context(|| format!("failed to read config: {path}"))?;
    let config: Config =
        toml::from_str(&contents).with_context(|| format!("failed to parse config: {path}"))?;
    config
        .validate()
        .with_context(|| format!("invalid config: {path}"))?;
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

    tracing::info!(
        zones = config.zones.len(),
        sensors = config.sensors.len(),
        "config applied"
    );

    Ok(())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- Helper: build a valid baseline config that passes validation ------

    fn valid_zone() -> ZoneEntry {
        ZoneEntry {
            zone_id: "z1".into(),
            name: "Zone 1".into(),
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

    fn valid_sensor() -> SensorEntry {
        SensorEntry {
            sensor_id: "node-a/s1".into(),
            node_id: "node-a".into(),
            zone_id: "z1".into(),
            raw_dry: 26000,
            raw_wet: 12000,
        }
    }

    fn valid_config() -> Config {
        Config {
            zones: vec![valid_zone()],
            sensors: vec![valid_sensor()],
        }
    }

    /// Assert validation fails and the error message contains `needle`.
    fn assert_validation_err(cfg: &Config, needle: &str) {
        let err = cfg.validate().unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains(needle),
            "expected error containing {needle:?}, got: {msg}"
        );
    }

    // -- Parsing ----------------------------------------------------------

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

    // -- Validation: valid configs pass -----------------------------------

    #[test]
    fn valid_config_passes() {
        valid_config().validate().unwrap();
    }

    #[test]
    fn empty_config_passes() {
        let cfg = Config {
            zones: vec![],
            sensors: vec![],
        };
        cfg.validate().unwrap();
    }

    #[test]
    fn multi_zone_multi_sensor_passes() {
        let cfg = Config {
            zones: vec![
                ZoneEntry {
                    zone_id: "z1".into(),
                    valve_gpio_pin: 17,
                    ..valid_zone()
                },
                ZoneEntry {
                    zone_id: "z2".into(),
                    name: "Zone 2".into(),
                    valve_gpio_pin: 27,
                    ..valid_zone()
                },
            ],
            sensors: vec![
                SensorEntry {
                    sensor_id: "node-a/s1".into(),
                    zone_id: "z1".into(),
                    ..valid_sensor()
                },
                SensorEntry {
                    sensor_id: "node-a/s2".into(),
                    zone_id: "z2".into(),
                    ..valid_sensor()
                },
            ],
        };
        cfg.validate().unwrap();
    }

    // -- Zone: identity ---------------------------------------------------

    #[test]
    fn zone_empty_id_rejected() {
        let mut cfg = valid_config();
        cfg.zones[0].zone_id = "".into();
        assert_validation_err(&cfg, "zone_id is empty");
    }

    #[test]
    fn zone_duplicate_id_rejected() {
        let mut cfg = valid_config();
        cfg.zones.push(ZoneEntry {
            valve_gpio_pin: 27, // different pin, same id
            ..valid_zone()
        });
        assert_validation_err(&cfg, "duplicate zone_id");
    }

    #[test]
    fn zone_empty_name_rejected() {
        let mut cfg = valid_config();
        cfg.zones[0].name = "  ".into();
        assert_validation_err(&cfg, "name is empty");
    }

    // -- Zone: moisture bounds --------------------------------------------

    #[test]
    fn zone_min_moisture_below_zero() {
        let mut cfg = valid_config();
        cfg.zones[0].min_moisture = -0.1;
        assert_validation_err(&cfg, "min_moisture");
    }

    #[test]
    fn zone_min_moisture_above_one() {
        let mut cfg = valid_config();
        cfg.zones[0].min_moisture = 1.01;
        assert_validation_err(&cfg, "min_moisture");
    }

    #[test]
    fn zone_target_moisture_below_zero() {
        let mut cfg = valid_config();
        cfg.zones[0].target_moisture = -0.1;
        assert_validation_err(&cfg, "target_moisture");
    }

    #[test]
    fn zone_target_moisture_above_one() {
        let mut cfg = valid_config();
        cfg.zones[0].target_moisture = 1.5;
        assert_validation_err(&cfg, "target_moisture");
    }

    #[test]
    fn zone_target_must_exceed_min() {
        let mut cfg = valid_config();
        cfg.zones[0].min_moisture = 0.5;
        cfg.zones[0].target_moisture = 0.5; // equal, not greater
        assert_validation_err(
            &cfg,
            "target_moisture (0.5) must be greater than min_moisture (0.5)",
        );
    }

    #[test]
    fn zone_target_less_than_min() {
        let mut cfg = valid_config();
        cfg.zones[0].min_moisture = 0.6;
        cfg.zones[0].target_moisture = 0.4;
        assert_validation_err(&cfg, "must be greater than min_moisture");
    }

    // -- Zone: timing values ----------------------------------------------

    #[test]
    fn zone_pulse_sec_zero() {
        let mut cfg = valid_config();
        cfg.zones[0].pulse_sec = 0;
        assert_validation_err(&cfg, "pulse_sec must be positive");
    }

    #[test]
    fn zone_pulse_sec_negative() {
        let mut cfg = valid_config();
        cfg.zones[0].pulse_sec = -5;
        assert_validation_err(&cfg, "pulse_sec must be positive");
    }

    #[test]
    fn zone_soak_min_zero() {
        let mut cfg = valid_config();
        cfg.zones[0].soak_min = 0;
        assert_validation_err(&cfg, "soak_min must be positive");
    }

    #[test]
    fn zone_max_open_sec_zero() {
        let mut cfg = valid_config();
        cfg.zones[0].max_open_sec_per_day = 0;
        assert_validation_err(&cfg, "max_open_sec_per_day must be positive");
    }

    #[test]
    fn zone_max_pulses_zero() {
        let mut cfg = valid_config();
        cfg.zones[0].max_pulses_per_day = 0;
        assert_validation_err(&cfg, "max_pulses_per_day must be positive");
    }

    #[test]
    fn zone_stale_timeout_zero() {
        let mut cfg = valid_config();
        cfg.zones[0].stale_timeout_min = 0;
        assert_validation_err(&cfg, "stale_timeout_min must be positive");
    }

    #[test]
    fn zone_pulse_exceeds_daily_max() {
        let mut cfg = valid_config();
        cfg.zones[0].pulse_sec = 200;
        cfg.zones[0].max_open_sec_per_day = 100;
        assert_validation_err(&cfg, "pulse_sec (200) exceeds max_open_sec_per_day (100)");
    }

    // -- Zone: GPIO whitelist ---------------------------------------------

    #[test]
    fn zone_gpio_pin_0_rejected() {
        let mut cfg = valid_config();
        cfg.zones[0].valve_gpio_pin = 0;
        assert_validation_err(&cfg, "not a valid BCM GPIO pin");
    }

    #[test]
    fn zone_gpio_pin_1_rejected() {
        let mut cfg = valid_config();
        cfg.zones[0].valve_gpio_pin = 1;
        assert_validation_err(&cfg, "not a valid BCM GPIO pin");
    }

    #[test]
    fn zone_gpio_pin_28_rejected() {
        let mut cfg = valid_config();
        cfg.zones[0].valve_gpio_pin = 28;
        assert_validation_err(&cfg, "not a valid BCM GPIO pin");
    }

    #[test]
    fn zone_gpio_negative_rejected() {
        let mut cfg = valid_config();
        cfg.zones[0].valve_gpio_pin = -1;
        assert_validation_err(&cfg, "not a valid BCM GPIO pin");
    }

    #[test]
    fn zone_gpio_boundary_2_accepted() {
        let mut cfg = valid_config();
        cfg.zones[0].valve_gpio_pin = 2;
        cfg.validate().unwrap();
    }

    #[test]
    fn zone_gpio_boundary_27_accepted() {
        let mut cfg = valid_config();
        cfg.zones[0].valve_gpio_pin = 27;
        cfg.validate().unwrap();
    }

    #[test]
    fn zone_duplicate_gpio_rejected() {
        let cfg = Config {
            zones: vec![
                ZoneEntry {
                    zone_id: "z1".into(),
                    valve_gpio_pin: 17,
                    ..valid_zone()
                },
                ZoneEntry {
                    zone_id: "z2".into(),
                    name: "Zone 2".into(),
                    valve_gpio_pin: 17, // same pin!
                    ..valid_zone()
                },
            ],
            sensors: vec![],
        };
        assert_validation_err(&cfg, "already used by another zone");
    }

    // -- Sensor: identity -------------------------------------------------

    #[test]
    fn sensor_empty_id_rejected() {
        let mut cfg = valid_config();
        cfg.sensors[0].sensor_id = "".into();
        assert_validation_err(&cfg, "sensor_id is empty");
    }

    #[test]
    fn sensor_duplicate_id_rejected() {
        let mut cfg = valid_config();
        cfg.sensors.push(valid_sensor());
        assert_validation_err(&cfg, "duplicate sensor_id");
    }

    #[test]
    fn sensor_empty_node_id_rejected() {
        let mut cfg = valid_config();
        cfg.sensors[0].node_id = " ".into();
        assert_validation_err(&cfg, "node_id is empty");
    }

    #[test]
    fn sensor_empty_zone_id_rejected() {
        let mut cfg = valid_config();
        cfg.sensors[0].zone_id = "".into();
        assert_validation_err(&cfg, "zone_id is empty");
    }

    #[test]
    fn sensor_unknown_zone_rejected() {
        let mut cfg = valid_config();
        cfg.sensors[0].zone_id = "nonexistent".into();
        assert_validation_err(&cfg, "does not match any defined zone");
    }

    // -- Sensor: ADC calibration ------------------------------------------

    #[test]
    fn sensor_raw_dry_negative() {
        let mut cfg = valid_config();
        cfg.sensors[0].raw_dry = -1;
        assert_validation_err(&cfg, "raw_dry -1 out of ADS1115 range");
    }

    #[test]
    fn sensor_raw_dry_too_high() {
        let mut cfg = valid_config();
        cfg.sensors[0].raw_dry = 40000;
        assert_validation_err(&cfg, "raw_dry 40000 out of ADS1115 range");
    }

    #[test]
    fn sensor_raw_wet_negative() {
        let mut cfg = valid_config();
        cfg.sensors[0].raw_wet = -100;
        assert_validation_err(&cfg, "raw_wet -100 out of ADS1115 range");
    }

    #[test]
    fn sensor_raw_wet_too_high() {
        let mut cfg = valid_config();
        cfg.sensors[0].raw_wet = 32768;
        assert_validation_err(&cfg, "raw_wet 32768 out of ADS1115 range");
    }

    #[test]
    fn sensor_raw_dry_equals_wet() {
        let mut cfg = valid_config();
        cfg.sensors[0].raw_dry = 15000;
        cfg.sensors[0].raw_wet = 15000;
        assert_validation_err(&cfg, "calibration range is zero");
    }

    // -- Multiple errors reported at once ---------------------------------

    #[test]
    fn multiple_errors_collected() {
        let cfg = Config {
            zones: vec![ZoneEntry {
                zone_id: "".into(),
                name: "".into(),
                min_moisture: -1.0,
                target_moisture: 2.0,
                pulse_sec: -1,
                soak_min: 0,
                max_open_sec_per_day: 0,
                max_pulses_per_day: 0,
                stale_timeout_min: 0,
                valve_gpio_pin: 0,
            }],
            sensors: vec![],
        };
        let err = cfg.validate().unwrap_err();
        let msg = format!("{err:#}");
        // Should report many errors, not bail after the first
        assert!(
            msg.contains("zone_id is empty"),
            "missing zone_id error in: {msg}"
        );
        assert!(
            msg.contains("min_moisture"),
            "missing moisture error in: {msg}"
        );
        assert!(
            msg.contains("not a valid BCM GPIO pin"),
            "missing gpio error in: {msg}"
        );
    }

    // -- DB integration ---------------------------------------------------

    #[tokio::test]
    async fn apply_seeds_database() {
        let db = Db::connect("sqlite::memory:").await.unwrap();
        db.migrate().await.unwrap();

        let config = valid_config();
        config.validate().unwrap();

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
