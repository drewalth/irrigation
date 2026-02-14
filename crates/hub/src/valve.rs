//! Valve control via GPIO. The `gpio` feature gates the real rppal driver;
//! without it, a mock implementation logs state changes to stderr.

use anyhow::Result;
use std::collections::HashMap;

#[cfg(feature = "gpio")]
use rppal::gpio::{Gpio, OutputPin};

// ---------------------------------------------------------------------------
// Real GPIO valve board (production — requires rppal + Raspberry Pi hardware)
// ---------------------------------------------------------------------------
#[cfg(feature = "gpio")]
pub(crate) struct ValveBoard {
    pins: HashMap<String, OutputPin>, // zone_id -> GPIO pin
    active_low: bool,                 // many relay boards are active-low
}

#[cfg(feature = "gpio")]
impl ValveBoard {
    pub(crate) fn new(zone_to_gpio: &[(String, u8)], active_low: bool) -> Result<Self> {
        let gpio = Gpio::new()?;
        let mut pins = HashMap::new();

        for (zone_id, pin_num) in zone_to_gpio {
            let mut pin = gpio.get(*pin_num)?.into_output();

            // Fail-safe: ensure "OFF" at startup
            if active_low {
                pin.set_high(); // active-low relay OFF
            } else {
                pin.set_low(); // active-high relay OFF
            }

            pins.insert(zone_id.clone(), pin);
        }

        Ok(Self { pins, active_low })
    }

    pub(crate) fn set(&mut self, zone_id: &str, on: bool) {
        if let Some(pin) = self.pins.get_mut(zone_id) {
            if self.active_low {
                // active-low relay: LOW = ON, HIGH = OFF
                if on {
                    pin.set_low()
                } else {
                    pin.set_high()
                }
            } else {
                // active-high relay: HIGH = ON, LOW = OFF
                if on {
                    pin.set_high()
                } else {
                    pin.set_low()
                }
            }
            eprintln!("valve zone={zone_id} set {}", if on { "ON" } else { "OFF" });
        } else {
            eprintln!("unknown zone_id '{zone_id}'");
        }
    }

    pub(crate) fn all_off(&mut self) {
        let keys: Vec<String> = self.pins.keys().cloned().collect();
        for k in keys {
            self.set(&k, false);
        }
    }
}

// ---------------------------------------------------------------------------
// Mock valve board (development — no hardware, logs state to stderr)
// ---------------------------------------------------------------------------
#[cfg(not(feature = "gpio"))]
pub(crate) struct ValveBoard {
    pub(super) zones: HashMap<String, bool>, // zone_id -> on/off state
}

#[cfg(not(feature = "gpio"))]
impl ValveBoard {
    pub(crate) fn new(zone_to_gpio: &[(String, u8)], _active_low: bool) -> Result<Self> {
        let mut zones = HashMap::new();
        for (zone_id, pin_num) in zone_to_gpio {
            eprintln!("[mock-gpio] registered zone={zone_id} (gpio {pin_num} — not wired)");
            zones.insert(zone_id.clone(), false);
        }
        eprintln!("[mock-gpio] valve board initialised (no hardware)");
        Ok(Self { zones })
    }

    pub(crate) fn set(&mut self, zone_id: &str, on: bool) {
        if let Some(state) = self.zones.get_mut(zone_id) {
            *state = on;
            eprintln!(
                "[mock-gpio] valve zone={zone_id} set {}",
                if on { "ON" } else { "OFF" }
            );
        } else {
            eprintln!("[mock-gpio] unknown zone_id '{zone_id}'");
        }
    }

    pub(crate) fn all_off(&mut self) {
        let keys: Vec<String> = self.zones.keys().cloned().collect();
        for k in keys {
            self.set(&k, false);
        }
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // -- ValveBoard (mock) --------------------------------------------------

    #[test]
    fn valve_board_new_registers_zones() {
        let zones = vec![("z1".to_string(), 17), ("z2".to_string(), 27)];
        let board = ValveBoard::new(&zones, true).unwrap();
        assert_eq!(board.zones.len(), 2);
    }

    #[test]
    fn valve_board_new_all_off() {
        let zones = vec![("z1".to_string(), 17)];
        let board = ValveBoard::new(&zones, true).unwrap();
        assert!(!board.zones["z1"]);
    }

    #[test]
    fn valve_board_set_on() {
        let zones = vec![("z1".to_string(), 17)];
        let mut board = ValveBoard::new(&zones, true).unwrap();
        board.set("z1", true);
        assert!(board.zones["z1"]);
    }

    #[test]
    fn valve_board_set_off() {
        let zones = vec![("z1".to_string(), 17)];
        let mut board = ValveBoard::new(&zones, true).unwrap();
        board.set("z1", true);
        board.set("z1", false);
        assert!(!board.zones["z1"]);
    }

    #[test]
    fn valve_board_all_off_resets_everything() {
        let zones = vec![("z1".to_string(), 17), ("z2".to_string(), 27)];
        let mut board = ValveBoard::new(&zones, true).unwrap();
        board.set("z1", true);
        board.set("z2", true);
        board.all_off();
        assert!(!board.zones["z1"]);
        assert!(!board.zones["z2"]);
    }

    #[test]
    fn valve_board_set_unknown_zone_does_not_panic() {
        let zones = vec![("z1".to_string(), 17)];
        let mut board = ValveBoard::new(&zones, true).unwrap();
        board.set("nonexistent", true); // should not panic
        assert_eq!(board.zones.len(), 1); // no new entry created
    }
}
