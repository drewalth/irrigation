//! ADS1115 16-bit ADC driver over I2C for soil moisture sensing.
//!
//! Reads single-ended channels at PGA ±4.096 V, 128 SPS, single-shot mode.
//! This matches the calibration values in `config.toml` (`raw_dry ≈ 26000`,
//! `raw_wet ≈ 12000`) for typical capacitive soil moisture sensors powered
//! from 3.3 V.

use rppal::i2c::I2c;
use std::{thread, time::Duration};

use crate::Reading;

// ── ADS1115 register addresses ──────────────────────────────────────────────

/// Conversion result register (read-only, 16-bit signed).
const REG_CONVERSION: u8 = 0x00;
/// Configuration register (read/write).
const REG_CONFIG: u8 = 0x01;

// ── Config register bit fields ──────────────────────────────────────────────
//
// Layout (MSB first):
//   [15]    OS       — write 1 to start single-shot conversion
//   [14:12] MUX      — input multiplexer (channel selection)
//   [11:9]  PGA      — programmable gain amplifier
//   [8]     MODE     — 0 = continuous, 1 = single-shot
//   [7:5]   DR       — data rate
//   [4]     COMP_MODE
//   [3]     COMP_POL
//   [2]     COMP_LAT
//   [1:0]   COMP_QUE — 11 = disable comparator (default)

/// Bits common to all channel reads:
///   OS=1 (start), PGA=001 (±4.096 V), MODE=1 (single-shot),
///   DR=100 (128 SPS), COMP_QUE=11 (comparator off).
const CONFIG_BASE: u16 = 0b1_000_001_1_100_0_0_0_11;

/// MUX values for single-ended reads (AINx vs GND).
///   AIN0: MUX=100, AIN1: MUX=101, AIN2: MUX=110, AIN3: MUX=111
const MUX_SHIFT: u8 = 12;
const MUX_SINGLE_ENDED: [u16; 4] = [0b100, 0b101, 0b110, 0b111];

/// Maximum valid ADS1115 channel index (0–3 for single-ended).
const MAX_CHANNEL: usize = 3;

/// Conversion time at 128 SPS is ~7.8 ms.  We wait 9 ms for margin.
const CONVERSION_WAIT: Duration = Duration::from_millis(9);

/// Bit 15 of the config register: conversion-ready flag when read.
const OS_READY_BIT: u16 = 1 << 15;

// ── Channel configuration ───────────────────────────────────────────────────

/// A mapping from an ADS1115 channel index (0–3) to a sensor ID string.
#[derive(Debug, Clone)]
pub struct ChannelMap {
    /// ADS1115 channel index (0 = AIN0, 1 = AIN1, …).
    pub channel: usize,
    /// Sensor ID published in MQTT readings (e.g. "s1").
    pub sensor_id: String,
}

/// Build the config register value for a single-ended read on `channel`.
fn config_for_channel(channel: usize) -> u16 {
    CONFIG_BASE | (MUX_SINGLE_ENDED[channel] << MUX_SHIFT)
}

// ── Driver ──────────────────────────────────────────────────────────────────

/// ADS1115 driver backed by `rppal::i2c`.
pub struct Ads1115 {
    i2c: I2c,
    channels: Vec<ChannelMap>,
}

impl Ads1115 {
    /// Open I2C bus 1 and configure for ADS1115 at `addr`.
    ///
    /// `channels` defines which ADS1115 inputs to read and how to label them.
    /// Panics if any channel index exceeds 3.
    pub fn new(addr: u16, channels: Vec<ChannelMap>) -> anyhow::Result<Self> {
        for ch in &channels {
            anyhow::ensure!(
                ch.channel <= MAX_CHANNEL,
                "ADS1115 channel {} out of range (0–{MAX_CHANNEL})",
                ch.channel,
            );
        }

        let mut i2c = I2c::new()?;
        i2c.set_slave_address(addr)?;

        tracing::info!(
            addr = format_args!("0x{addr:02x}"),
            channels = ?channels,
            "ads1115 initialised"
        );

        Ok(Self { i2c, channels })
    }

    /// Perform a single-shot read on `channel`, returning the raw 16-bit
    /// signed value (0–32767 for single-ended).
    fn read_channel(&mut self, channel: usize) -> anyhow::Result<i16> {
        let config = config_for_channel(channel);
        let config_bytes = config.to_be_bytes();

        // Write config register to start conversion.
        self.i2c.block_write(REG_CONFIG, &config_bytes)?;

        // Wait for conversion to complete.
        thread::sleep(CONVERSION_WAIT);

        // Poll the OS bit to confirm conversion is done.  Normally one wait
        // is enough at 128 SPS; we retry briefly to be safe.
        for _ in 0..3 {
            let mut buf = [0u8; 2];
            self.i2c.block_read(REG_CONFIG, &mut buf)?;
            let status = u16::from_be_bytes(buf);
            if status & OS_READY_BIT != 0 {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }

        // Read the conversion result.
        let mut buf = [0u8; 2];
        self.i2c.block_read(REG_CONVERSION, &mut buf)?;
        Ok(i16::from_be_bytes(buf))
    }

    /// Read all configured channels and return a `Vec<Reading>`.
    ///
    /// On per-channel failure the channel is skipped (logged, not fatal).
    pub fn read_all(&mut self) -> Vec<Reading> {
        let mut readings = Vec::with_capacity(self.channels.len());

        for ch in &self.channels.clone() {
            match self.read_channel(ch.channel) {
                Ok(raw) => {
                    // Single-ended reads are non-negative; clamp defensively
                    // against bus corruption.
                    let clamped = (raw as i32).clamp(0, 32767);
                    readings.push(Reading {
                        sensor_id: ch.sensor_id.clone(),
                        raw: clamped,
                    });
                }
                Err(e) => {
                    tracing::error!(
                        channel = ch.channel,
                        sensor_id = %ch.sensor_id,
                        "adc read failed: {e}"
                    );
                }
            }
        }

        readings
    }
}

// ── Channel parsing ─────────────────────────────────────────────────────────

/// Parse the `SENSOR_CHANNELS` environment variable into a channel map.
///
/// Format: comma-separated ADS1115 channel indices, e.g. `"0,1"`.
/// Channel 0 → sensor_id "s1", channel 1 → "s2", etc.
///
/// Defaults to `"0,1"` if the variable is unset or empty.
pub fn parse_channels(env_val: &str) -> anyhow::Result<Vec<ChannelMap>> {
    let input = if env_val.is_empty() { "0,1" } else { env_val };
    let mut channels = Vec::new();

    for (i, token) in input.split(',').enumerate() {
        let ch: usize = token
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("invalid channel in SENSOR_CHANNELS: {token:?}"))?;
        anyhow::ensure!(
            ch <= MAX_CHANNEL,
            "channel {ch} in SENSOR_CHANNELS exceeds maximum ({MAX_CHANNEL})"
        );
        channels.push(ChannelMap {
            channel: ch,
            sensor_id: format!("s{}", i + 1),
        });
    }

    anyhow::ensure!(!channels.is_empty(), "SENSOR_CHANNELS produced no channels");
    Ok(channels)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // -- Config register construction -----------------------------------------

    #[test]
    fn config_register_channel_a0() {
        // AIN0 vs GND: MUX = 100 → bits [14:12] = 0b100
        let cfg = config_for_channel(0);
        assert_eq!(cfg, 0xC383, "A0 config: {cfg:#06x}");
    }

    #[test]
    fn config_register_channel_a1() {
        let cfg = config_for_channel(1);
        assert_eq!(cfg, 0xD383, "A1 config: {cfg:#06x}");
    }

    #[test]
    fn config_register_channel_a2() {
        let cfg = config_for_channel(2);
        assert_eq!(cfg, 0xE383, "A2 config: {cfg:#06x}");
    }

    #[test]
    fn config_register_channel_a3() {
        let cfg = config_for_channel(3);
        assert_eq!(cfg, 0xF383, "A3 config: {cfg:#06x}");
    }

    #[test]
    fn config_base_has_correct_pga() {
        // PGA bits [11:9] should be 001 for ±4.096 V.
        let pga = (CONFIG_BASE >> 9) & 0b111;
        assert_eq!(pga, 0b001, "PGA should be ±4.096 V");
    }

    #[test]
    fn config_base_is_single_shot() {
        // MODE bit [8] should be 1 for single-shot.
        let mode = (CONFIG_BASE >> 8) & 1;
        assert_eq!(mode, 1, "MODE should be single-shot");
    }

    #[test]
    fn config_base_data_rate_128sps() {
        // DR bits [7:5] should be 100 for 128 SPS.
        let dr = (CONFIG_BASE >> 5) & 0b111;
        assert_eq!(dr, 0b100, "DR should be 128 SPS");
    }

    #[test]
    fn config_base_starts_conversion() {
        // OS bit [15] should be 1 to start a conversion.
        let os = (CONFIG_BASE >> 15) & 1;
        assert_eq!(os, 1, "OS should be set to start conversion");
    }

    // -- Channel parsing ------------------------------------------------------

    #[test]
    fn parse_channels_default() {
        let channels = parse_channels("").unwrap();
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[0].channel, 0);
        assert_eq!(channels[0].sensor_id, "s1");
        assert_eq!(channels[1].channel, 1);
        assert_eq!(channels[1].sensor_id, "s2");
    }

    #[test]
    fn parse_channels_explicit() {
        let channels = parse_channels("2,3,0").unwrap();
        assert_eq!(channels.len(), 3);
        assert_eq!(channels[0].channel, 2);
        assert_eq!(channels[0].sensor_id, "s1");
        assert_eq!(channels[1].channel, 3);
        assert_eq!(channels[1].sensor_id, "s2");
        assert_eq!(channels[2].channel, 0);
        assert_eq!(channels[2].sensor_id, "s3");
    }

    #[test]
    fn parse_channels_single() {
        let channels = parse_channels("0").unwrap();
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].channel, 0);
        assert_eq!(channels[0].sensor_id, "s1");
    }

    #[test]
    fn parse_channels_with_whitespace() {
        let channels = parse_channels(" 0 , 1 ").unwrap();
        assert_eq!(channels.len(), 2);
        assert_eq!(channels[0].channel, 0);
        assert_eq!(channels[1].channel, 1);
    }

    #[test]
    fn parse_channels_invalid_number() {
        assert!(parse_channels("abc").is_err());
    }

    #[test]
    fn parse_channels_out_of_range() {
        assert!(parse_channels("0,4").is_err());
    }

    #[test]
    fn parse_channels_negative() {
        assert!(parse_channels("-1").is_err());
    }
}
