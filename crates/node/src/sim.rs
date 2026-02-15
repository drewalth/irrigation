//! Stateful soil moisture sensor simulator for local development.
//!
//! Models realistic capacitive sensor behaviour:
//! - Temporal coherence via random walk with mean reversion
//! - Gradual drying drift (evaporation)
//! - Per-reading ADC electronic noise
//! - Occasional spikes (sensor flakiness)
//! - Diurnal (day/night) cycle
//! - Per-sensor calibration offsets
//! - Closed-loop watering response (moisture increases when valve is open)

use std::fmt;

// ---------------------------------------------------------------------------
// Gaussian approximation (no extra dependency)
// ---------------------------------------------------------------------------

/// Approximate a sample from N(0,1) using the Irwin-Hall method:
/// sum of 12 uniform [0,1) values minus 6.
fn approx_std_normal() -> f64 {
    let mut sum: f64 = 0.0;
    for _ in 0..12 {
        sum += fastrand::f64();
    }
    sum - 6.0
}

/// Sample from N(mean, sigma).
fn gaussian(mean: f64, sigma: f64) -> f64 {
    mean + sigma * approx_std_normal()
}

// ---------------------------------------------------------------------------
// Scenario presets
// ---------------------------------------------------------------------------

/// Pre-configured simulation profiles selectable via `SIM_SCENARIO` env var.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scenario {
    /// Starts mid-range, slow drift toward dry.  Moderate noise.  ~3% spike
    /// rate.  Realistic steady-state for a warm day.
    Drying,
    /// Hovers near the centre.  Low noise, rare spikes.  Good for testing UI
    /// without triggering watering.
    Stable,
    /// High noise sigma, ~10% spike rate, larger spike magnitude.  Tests
    /// hub's plausibility filter and averaging robustness.
    Flaky,
    /// Starts near wet end.  Very slow drying.  Tests that scheduler
    /// correctly does nothing when moisture is adequate.
    Wet,
}

impl Scenario {
    pub fn from_str_lossy(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "stable" => Self::Stable,
            "flaky" => Self::Flaky,
            "wet" => Self::Wet,
            _ => Self::Drying, // default
        }
    }
}

impl fmt::Display for Scenario {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Drying => write!(f, "drying"),
            Self::Stable => write!(f, "stable"),
            Self::Flaky => write!(f, "flaky"),
            Self::Wet => write!(f, "wet"),
        }
    }
}

// ---------------------------------------------------------------------------
// Per-sensor state
// ---------------------------------------------------------------------------

/// Internal state for a single simulated sensor channel.
struct SensorState {
    /// Current "true" soil moisture in ADC units.  Evolves each tick.
    base: f64,
    /// Permanent per-sensor calibration offset (ADC units).  Models the fact
    /// that two sensors in the same soil will not read identically.
    offset: f64,
    /// Per-sensor noise sigma (ADC units).
    noise_sigma: f64,
}

// ---------------------------------------------------------------------------
// Main simulator
// ---------------------------------------------------------------------------

/// Stateful simulator producing realistic soil moisture ADC readings.
pub struct SoilMoistureSim {
    sensors: Vec<SensorState>,

    // Calibration endpoints (from config.toml)
    raw_dry: f64,
    raw_wet: f64,

    // Random walk parameters
    drift_per_sample: f64,
    walk_sigma: f64,
    mean_reversion: f64,
    center: f64,

    // Spike parameters
    spike_prob: f32,
    spike_sigma: f64,

    // Diurnal cycle
    diurnal_amplitude: f64,
    diurnal_period_s: f64,

    // Watering response
    watering: bool,
    wet_rate: f64,
}

impl SoilMoistureSim {
    /// Create a new simulator for `sensor_count` channels.
    ///
    /// `raw_dry` / `raw_wet` should match the sensor calibration in
    /// `config.toml` (typically 26000 / 12000 for an ADS1115).
    ///
    /// `diurnal_period_s` controls the day/night cycle length.  Use 600
    /// (10 min) for fast dev iteration or 86400 for real-time.
    pub fn new(
        scenario: Scenario,
        sensor_count: usize,
        raw_dry: f64,
        raw_wet: f64,
        diurnal_period_s: f64,
    ) -> Self {
        let range = raw_dry - raw_wet; // typically 14000
        let center = (raw_dry + raw_wet) / 2.0; // ~19000

        let (drift, walk_sigma, mean_rev, noise_sigma, spike_prob, spike_sigma, start_frac) =
            match scenario {
                // start_frac: 0.0 = at raw_wet (wettest), 1.0 = at raw_dry (driest)
                Scenario::Drying => (15.0, 150.0, 0.02, 80.0, 0.03_f32, 2000.0, 0.5),
                Scenario::Stable => (2.0, 60.0, 0.05, 40.0, 0.005, 1000.0, 0.5),
                Scenario::Flaky => (10.0, 250.0, 0.02, 200.0, 0.10, 3000.0, 0.5),
                Scenario::Wet => (3.0, 80.0, 0.02, 60.0, 0.02, 1500.0, 0.2),
            };

        // Starting base in ADC units.  raw_wet + frac * range.
        let start_base = raw_wet + start_frac * range;

        // Per-sensor: randomise initial base slightly and assign a permanent
        // calibration offset so sensors diverge naturally.
        let sensors = (0..sensor_count)
            .map(|_| {
                let jitter = gaussian(0.0, range * 0.03); // +-~3% of range
                let offset = gaussian(0.0, range * 0.02); // permanent shift
                let sensor_noise = noise_sigma * (1.0 + 0.2 * approx_std_normal()).max(0.3);
                SensorState {
                    base: (start_base + jitter).clamp(raw_wet, raw_dry),
                    offset,
                    noise_sigma: sensor_noise,
                }
            })
            .collect();

        Self {
            sensors,
            raw_dry,
            raw_wet,
            drift_per_sample: drift,
            walk_sigma,
            mean_reversion: mean_rev,
            center,
            spike_prob,
            spike_sigma,
            diurnal_amplitude: range * 0.06, // ~6% of range (~840 for 14000)
            diurnal_period_s,
            watering: false,
            wet_rate: -300.0,
        }
    }

    /// Inform the simulator whether a watering valve is currently open.
    pub fn set_watering(&mut self, active: bool) {
        self.watering = active;
    }

    /// Produce the next ADC reading for the sensor at `index`.
    ///
    /// Call this once per sensor per sampling tick.  The internal base value
    /// evolves with each call, so the order and frequency of calls matters.
    pub fn sample(&mut self, index: usize) -> i32 {
        let sensor = &mut self.sensors[index];

        // -- Evolve the base value ----------------------------------------

        // Mean reversion: pull toward centre
        let pull = self.mean_reversion * (self.center - sensor.base);

        // Random walk step
        let walk = gaussian(0.0, self.walk_sigma);

        // Drying drift (positive = toward raw_dry = drier)
        let drift = self.drift_per_sample;

        // Watering effect (negative = toward raw_wet = wetter)
        let wet = if self.watering { self.wet_rate } else { 0.0 };

        sensor.base = (sensor.base + drift + pull + walk + wet)
            .clamp(self.raw_wet - 500.0, self.raw_dry + 500.0);

        // -- Build the instantaneous reading ------------------------------

        // Diurnal offset: sinusoidal, peaks at "afternoon" (period/2).
        let now_s = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64();
        let phase = 2.0 * std::f64::consts::PI * now_s / self.diurnal_period_s;
        let diurnal = self.diurnal_amplitude * phase.sin();

        // Electronic noise
        let noise = gaussian(0.0, sensor.noise_sigma);

        // Occasional spike (sensor flakiness)
        let spike = if fastrand::f32() < self.spike_prob {
            gaussian(0.0, self.spike_sigma)
        } else {
            0.0
        };

        let reading = sensor.base + sensor.offset + diurnal + noise + spike;

        // Clamp to physically possible ADC range (ADS1115: 0..32767) and
        // round to integer.
        reading.round().clamp(0.0, 32767.0) as i32
    }

    /// Number of sensor channels in this simulator.
    pub fn sensor_count(&self) -> usize {
        self.sensors.len()
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: collect N samples from sensor 0.
    fn collect_samples(sim: &mut SoilMoistureSim, n: usize) -> Vec<i32> {
        (0..n).map(|_| sim.sample(0)).collect()
    }

    #[test]
    fn readings_within_adc_range() {
        let mut sim = SoilMoistureSim::new(Scenario::Drying, 2, 26000.0, 12000.0, 600.0);
        for _ in 0..500 {
            for i in 0..2 {
                let v = sim.sample(i);
                assert!((0..=32767).contains(&v), "ADC out of range: {v}");
            }
        }
    }

    #[test]
    fn temporal_coherence() {
        // Consecutive readings should be much closer than the full range.
        let mut sim = SoilMoistureSim::new(Scenario::Stable, 1, 26000.0, 12000.0, 600.0);
        let samples = collect_samples(&mut sim, 100);
        let max_jump: i32 = samples
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .max()
            .unwrap();
        // With stable scenario the max jump should be well under the full
        // 14000 range.  Allow up to 5000 to account for rare spikes.
        assert!(
            max_jump < 5000,
            "max consecutive jump too large: {max_jump}"
        );
    }

    #[test]
    fn per_sensor_variation() {
        // Two sensors should produce different readings.
        let mut sim = SoilMoistureSim::new(Scenario::Drying, 2, 26000.0, 12000.0, 600.0);
        let mut diffs = 0_u32;
        for _ in 0..50 {
            let a = sim.sample(0);
            let b = sim.sample(1);
            if a != b {
                diffs += 1;
            }
        }
        // Extremely unlikely that all 50 pairs are identical.
        assert!(diffs > 0, "sensors should diverge");
    }

    #[test]
    fn watering_decreases_readings() {
        // When watering is active, readings should trend downward (wetter =
        // lower ADC).
        let mut sim = SoilMoistureSim::new(Scenario::Drying, 1, 26000.0, 12000.0, 600.0);

        // Warm up and record baseline.
        for _ in 0..20 {
            sim.sample(0);
        }
        let before: f64 = (0..20).map(|_| sim.sample(0) as f64).sum::<f64>() / 20.0;

        sim.set_watering(true);
        // Let watering run for many ticks.
        for _ in 0..50 {
            sim.sample(0);
        }
        let after: f64 = (0..20).map(|_| sim.sample(0) as f64).sum::<f64>() / 20.0;

        assert!(
            after < before,
            "watering should decrease readings: before={before:.0} after={after:.0}"
        );
    }

    #[test]
    fn flaky_scenario_has_more_variation() {
        // Flaky should have higher variance than stable.
        fn variance(sim: &mut SoilMoistureSim, n: usize) -> f64 {
            let samples = collect_samples(sim, n);
            let mean = samples.iter().map(|&v| v as f64).sum::<f64>() / n as f64;
            samples.iter().map(|&v| (v as f64 - mean).powi(2)).sum::<f64>() / n as f64
        }

        let mut stable = SoilMoistureSim::new(Scenario::Stable, 1, 26000.0, 12000.0, 600.0);
        let mut flaky = SoilMoistureSim::new(Scenario::Flaky, 1, 26000.0, 12000.0, 600.0);

        let var_stable = variance(&mut stable, 200);
        let var_flaky = variance(&mut flaky, 200);

        assert!(
            var_flaky > var_stable,
            "flaky variance ({var_flaky:.0}) should exceed stable ({var_stable:.0})"
        );
    }

    #[test]
    fn scenario_from_str_lossy() {
        assert_eq!(Scenario::from_str_lossy("drying"), Scenario::Drying);
        assert_eq!(Scenario::from_str_lossy("STABLE"), Scenario::Stable);
        assert_eq!(Scenario::from_str_lossy("Flaky"), Scenario::Flaky);
        assert_eq!(Scenario::from_str_lossy("wet"), Scenario::Wet);
        assert_eq!(Scenario::from_str_lossy("unknown"), Scenario::Drying);
        assert_eq!(Scenario::from_str_lossy(""), Scenario::Drying);
    }

    #[test]
    fn scenario_display() {
        assert_eq!(Scenario::Drying.to_string(), "drying");
        assert_eq!(Scenario::Stable.to_string(), "stable");
        assert_eq!(Scenario::Flaky.to_string(), "flaky");
        assert_eq!(Scenario::Wet.to_string(), "wet");
    }

    #[test]
    fn wet_scenario_starts_low() {
        // Wet scenario should start near the wet end (lower ADC values).
        let mut sim = SoilMoistureSim::new(Scenario::Wet, 1, 26000.0, 12000.0, 600.0);
        let avg: f64 = (0..10).map(|_| sim.sample(0) as f64).sum::<f64>() / 10.0;
        let midpoint = (26000.0 + 12000.0) / 2.0;
        assert!(
            avg < midpoint,
            "wet scenario should start below midpoint: avg={avg:.0} mid={midpoint:.0}"
        );
    }

    #[test]
    fn approx_std_normal_has_zero_mean() {
        let n = 5000;
        let sum: f64 = (0..n).map(|_| approx_std_normal()).sum();
        let mean = sum / n as f64;
        // Mean should be close to zero.  With n=5000 the std error is
        // 1/sqrt(5000) ≈ 0.014, so ±0.1 is generous.
        assert!(
            mean.abs() < 0.15,
            "approx_std_normal mean should be near zero: {mean}"
        );
    }
}
