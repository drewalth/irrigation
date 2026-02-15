//! TOML config file loading, validation, and database seeding for zones and
//! sensors.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::db::{Db, SensorConfig, ZoneConfig};

// ---------------------------------------------------------------------------
// Operation mode
// ---------------------------------------------------------------------------

/// Global operation mode for the irrigation system.
///
/// - `Auto` (default): full irrigation control — the scheduler opens/closes
///   valves based on soil moisture readings.
/// - `Monitor`: soil moisture monitoring only — no valve actuation.  The
///   scheduler still evaluates moisture and records low-moisture alert events
///   in the event ring buffer, but never publishes ON commands.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OperationMode {
    #[default]
    Auto,
    Monitor,
}

// ---------------------------------------------------------------------------
// Config file structures
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct Config {
    /// Operation mode: `auto` (default) or `monitor`.
    #[serde(default)]
    pub mode: OperationMode,
    /// Maximum number of valves that can be open simultaneously. Prevents
    /// 12 V power supply brown-out when driving many solenoid relay channels.
    #[serde(default = "default_max_concurrent_valves")]
    pub max_concurrent_valves: usize,
    #[serde(default)]
    pub zones: Vec<ZoneEntry>,
    #[serde(default)]
    pub sensors: Vec<SensorEntry>,
}

fn default_max_concurrent_valves() -> usize {
    2
}

#[derive(Debug, Deserialize)]
pub struct ZoneEntry {
    pub zone_id: String,
    pub name: String,
    pub min_moisture: f32,
    pub target_moisture: f32,
    #[serde(default = "default_pulse_sec")]
    pub pulse_sec: i64,
    #[serde(default = "default_soak_min")]
    pub soak_min: i64,
    #[serde(default = "default_max_open_sec_per_day")]
    pub max_open_sec_per_day: i64,
    #[serde(default = "default_max_pulses_per_day")]
    pub max_pulses_per_day: i64,
    pub stale_timeout_min: i64,
    #[serde(default)]
    pub valve_gpio_pin: i64,
}

fn default_pulse_sec() -> i64 {
    30
}
fn default_soak_min() -> i64 {
    20
}
fn default_max_open_sec_per_day() -> i64 {
    180
}
fn default_max_pulses_per_day() -> i64 {
    6
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

/// BCM GPIO pins safe for general-purpose relay driving on the Raspberry Pi
/// 40-pin header. Excludes:
///   - GPIO 0-1: ID EEPROM (must never be used)
///   - GPIO 2-3: I2C1 (SDA/SCL) with hard-wired 1.8k pull-ups — cannot
///     reliably drive active-low relay optocouplers
///   - GPIO 7-11: SPI0 (CE1, CE0, MISO, MOSI, SCLK)
///   - GPIO 14-15: UART0 (TX/RX) — used for serial console
///   - GPIO 28+: not exposed on the standard header
const VALID_GPIO_PINS: &[i64] = &[
    4, 5, 6, 12, 13, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27,
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

        // max_concurrent_valves is only relevant in auto mode.
        if self.mode == OperationMode::Auto && self.max_concurrent_valves == 0 {
            errors.push("max_concurrent_valves must be at least 1".to_string());
        }

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
        let is_auto = self.mode == OperationMode::Auto;

        for (i, z) in self.zones.iter().enumerate() {
            let ctx = || {
                if z.zone_id.is_empty() {
                    format!("zones[{i}]")
                } else {
                    format!("zone '{}'", z.zone_id)
                }
            };

            // ── Identity (always validated) ─────────────────────
            if z.zone_id.trim().is_empty() {
                errors.push(format!("{}: zone_id is empty", ctx()));
            } else if !seen_ids.insert(&z.zone_id) {
                errors.push(format!("{}: duplicate zone_id", ctx()));
            }

            if z.name.trim().is_empty() {
                errors.push(format!("{}: name is empty", ctx()));
            }

            // ── Moisture bounds (always validated) ───────────────
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

            // ── Valve timing values (auto mode only) ─────────────
            if is_auto {
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
            }

            // stale_timeout_min is needed in both modes (staleness detection).
            if z.stale_timeout_min <= 0 {
                errors.push(format!(
                    "{}: stale_timeout_min must be positive, got {}",
                    ctx(),
                    z.stale_timeout_min
                ));
            }

            // pulse_sec cannot exceed the daily maximum (auto mode only).
            if is_auto
                && z.pulse_sec > 0
                && z.max_open_sec_per_day > 0
                && z.pulse_sec > z.max_open_sec_per_day
            {
                errors.push(format!(
                    "{}: pulse_sec ({}) exceeds max_open_sec_per_day ({})",
                    ctx(),
                    z.pulse_sec,
                    z.max_open_sec_per_day
                ));
            }

            // ── GPIO pin whitelist (auto mode only) ──────────────
            if is_auto {
                if !VALID_GPIO_PINS.contains(&z.valve_gpio_pin) {
                    errors.push(format!(
                        "{}: valve_gpio_pin {} is not a safe GPIO pin (allowed: {:?})",
                        ctx(),
                        z.valve_gpio_pin,
                        VALID_GPIO_PINS,
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
            mode: OperationMode::Auto,
            max_concurrent_valves: 2,
            zones: vec![valid_zone()],
            sensors: vec![valid_sensor()],
        }
    }

    fn monitor_config() -> Config {
        Config {
            mode: OperationMode::Monitor,
            max_concurrent_valves: 2,
            zones: vec![ZoneEntry {
                zone_id: "z1".into(),
                name: "Zone 1".into(),
                min_moisture: 0.3,
                target_moisture: 0.5,
                pulse_sec: 0,            // irrelevant in monitor mode
                soak_min: 0,             // irrelevant in monitor mode
                max_open_sec_per_day: 0, // irrelevant in monitor mode
                max_pulses_per_day: 0,   // irrelevant in monitor mode
                stale_timeout_min: 30,
                valve_gpio_pin: 0, // irrelevant in monitor mode
            }],
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
            mode: OperationMode::Auto,
            max_concurrent_valves: 2,
            zones: vec![],
            sensors: vec![],
        };
        cfg.validate().unwrap();
    }

    #[test]
    fn multi_zone_multi_sensor_passes() {
        let cfg = Config {
            mode: OperationMode::Auto,
            max_concurrent_valves: 2,
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
        assert_validation_err(&cfg, "not a safe GPIO pin");
    }

    #[test]
    fn zone_gpio_pin_1_rejected() {
        let mut cfg = valid_config();
        cfg.zones[0].valve_gpio_pin = 1;
        assert_validation_err(&cfg, "not a safe GPIO pin");
    }

    #[test]
    fn zone_gpio_pin_28_rejected() {
        let mut cfg = valid_config();
        cfg.zones[0].valve_gpio_pin = 28;
        assert_validation_err(&cfg, "not a safe GPIO pin");
    }

    #[test]
    fn zone_gpio_negative_rejected() {
        let mut cfg = valid_config();
        cfg.zones[0].valve_gpio_pin = -1;
        assert_validation_err(&cfg, "not a safe GPIO pin");
    }

    #[test]
    fn zone_gpio_i2c_pin_2_rejected() {
        let mut cfg = valid_config();
        cfg.zones[0].valve_gpio_pin = 2;
        assert_validation_err(&cfg, "not a safe GPIO pin");
    }

    #[test]
    fn zone_gpio_i2c_pin_3_rejected() {
        let mut cfg = valid_config();
        cfg.zones[0].valve_gpio_pin = 3;
        assert_validation_err(&cfg, "not a safe GPIO pin");
    }

    #[test]
    fn zone_gpio_spi_pin_8_rejected() {
        let mut cfg = valid_config();
        cfg.zones[0].valve_gpio_pin = 8;
        assert_validation_err(&cfg, "not a safe GPIO pin");
    }

    #[test]
    fn zone_gpio_uart_pin_14_rejected() {
        let mut cfg = valid_config();
        cfg.zones[0].valve_gpio_pin = 14;
        assert_validation_err(&cfg, "not a safe GPIO pin");
    }

    #[test]
    fn zone_gpio_boundary_4_accepted() {
        let mut cfg = valid_config();
        cfg.zones[0].valve_gpio_pin = 4;
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
            mode: OperationMode::Auto,
            max_concurrent_valves: 2,
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
            mode: OperationMode::Auto,
            max_concurrent_valves: 2,
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
            msg.contains("not a safe GPIO pin"),
            "missing gpio error in: {msg}"
        );
    }

    // -- max_concurrent_valves --------------------------------------------

    #[test]
    fn max_concurrent_valves_zero_rejected_auto_mode() {
        let cfg = Config {
            max_concurrent_valves: 0,
            ..valid_config()
        };
        assert_validation_err(&cfg, "max_concurrent_valves must be at least 1");
    }

    #[test]
    fn max_concurrent_valves_zero_allowed_monitor_mode() {
        let cfg = Config {
            max_concurrent_valves: 0,
            ..monitor_config()
        };
        cfg.validate().unwrap();
    }

    #[test]
    fn max_concurrent_valves_one_accepted() {
        let cfg = Config {
            max_concurrent_valves: 1,
            ..valid_config()
        };
        cfg.validate().unwrap();
    }

    #[test]
    fn max_concurrent_valves_defaults_to_two_in_toml() {
        // When not specified in TOML, should default to 2.
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.max_concurrent_valves, 2);
    }

    #[test]
    fn max_concurrent_valves_parsed_from_toml() {
        let toml_str = "max_concurrent_valves = 4\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.max_concurrent_valves, 4);
    }

    // -- Operation mode ---------------------------------------------------

    #[test]
    fn parse_mode_auto_from_toml() {
        let toml_str = "mode = \"auto\"\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.mode, OperationMode::Auto);
    }

    #[test]
    fn parse_mode_monitor_from_toml() {
        let toml_str = "mode = \"monitor\"\n";
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.mode, OperationMode::Monitor);
    }

    #[test]
    fn default_mode_is_auto_when_omitted() {
        let config: Config = toml::from_str("").unwrap();
        assert_eq!(config.mode, OperationMode::Auto);
    }

    #[test]
    fn invalid_mode_string_rejected() {
        let toml_str = "mode = \"turbo\"\n";
        let result: Result<Config, _> = toml::from_str(toml_str);
        assert!(result.is_err());
    }

    #[test]
    fn monitor_mode_minimal_zone_config_passes() {
        // Monitor mode: valve-specific fields default to zero/sensible values.
        // Validation should pass even with zero pulse_sec, valve_gpio_pin=0, etc.
        monitor_config().validate().unwrap();
    }

    #[test]
    fn monitor_mode_skips_gpio_validation() {
        let mut cfg = monitor_config();
        cfg.zones[0].valve_gpio_pin = 0; // would fail in auto mode
        cfg.validate().unwrap();
    }

    #[test]
    fn monitor_mode_skips_pulse_sec_validation() {
        let mut cfg = monitor_config();
        cfg.zones[0].pulse_sec = 0; // would fail in auto mode
        cfg.validate().unwrap();
    }

    #[test]
    fn monitor_mode_still_validates_identity() {
        let mut cfg = monitor_config();
        cfg.zones[0].zone_id = "".into();
        assert_validation_err(&cfg, "zone_id is empty");
    }

    #[test]
    fn monitor_mode_still_validates_moisture_bounds() {
        let mut cfg = monitor_config();
        cfg.zones[0].min_moisture = -0.1;
        assert_validation_err(&cfg, "min_moisture");
    }

    #[test]
    fn monitor_mode_still_validates_stale_timeout() {
        let mut cfg = monitor_config();
        cfg.zones[0].stale_timeout_min = 0;
        assert_validation_err(&cfg, "stale_timeout_min must be positive");
    }

    #[test]
    fn auto_mode_still_requires_positive_pulse_sec() {
        let mut cfg = valid_config();
        cfg.zones[0].pulse_sec = 0;
        assert_validation_err(&cfg, "pulse_sec must be positive");
    }

    #[test]
    fn auto_mode_still_requires_valid_gpio() {
        let mut cfg = valid_config();
        cfg.zones[0].valve_gpio_pin = 0;
        assert_validation_err(&cfg, "not a safe GPIO pin");
    }

    #[test]
    fn monitor_mode_parses_minimal_zone_toml() {
        let toml_str = r#"
mode = "monitor"

[[zones]]
zone_id = "z1"
name = "Zone 1"
min_moisture = 0.3
target_moisture = 0.5
stale_timeout_min = 30

[[sensors]]
sensor_id = "node-a/s1"
node_id = "node-a"
zone_id = "z1"
raw_dry = 26000
raw_wet = 12000
"#;
        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(config.mode, OperationMode::Monitor);
        assert_eq!(config.zones[0].pulse_sec, 30); // serde default
        assert_eq!(config.zones[0].valve_gpio_pin, 0); // serde default
        config.validate().unwrap();
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
