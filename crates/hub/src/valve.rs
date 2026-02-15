//! Valve control via GPIO. The `gpio` feature gates the real rppal driver;
//! without it, a mock implementation logs state changes.

use anyhow::Result;
use std::collections::HashMap;
use tracing::{info, warn};

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
            // Atomic pin init: set the correct OFF level *during* the
            // output-mode switch so the relay never sees a brief glitch.
            // (into_output() defaults to LOW, which activates active-low relays.)
            let mut pin = if active_low {
                gpio.get(*pin_num)?.into_output_high() // active-low: OFF = HIGH
            } else {
                gpio.get(*pin_num)?.into_output_low() // active-high: OFF = LOW
            };

            // Prevent rppal from resetting the pin to input mode (floating)
            // when OutputPin is dropped.  Our Drop impl already calls all_off()
            // to drive pins to the safe OFF level — floating after that would
            // risk relay chatter on boards with weak pull-ups.
            pin.set_reset_on_drop(false);

            pins.insert(zone_id.clone(), pin);
        }

        Ok(Self { pins, active_low })
    }

    pub(crate) fn set(&mut self, zone_id: &str, on: bool) {
        if let Some(pin) = self.pins.get_mut(zone_id) {
            if self.active_low {
                if on {
                    pin.set_low()
                } else {
                    pin.set_high()
                }
            } else {
                if on {
                    pin.set_high()
                } else {
                    pin.set_low()
                }
            }
            info!(zone = %zone_id, state = if on { "ON" } else { "OFF" }, "valve set");
        } else {
            warn!(zone = %zone_id, "unknown zone_id");
        }
    }

    pub(crate) fn all_off(&mut self) {
        let keys: Vec<String> = self.pins.keys().cloned().collect();
        for k in keys {
            self.set(&k, false);
        }
    }
}

#[cfg(feature = "gpio")]
impl Drop for ValveBoard {
    fn drop(&mut self) {
        // Safety net: ensure all relays are de-energized when dropped.
        self.all_off();
    }
}

// ---------------------------------------------------------------------------
// Mock valve board (development — no hardware, logs state)
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
            info!(zone = %zone_id, gpio = pin_num, "[mock] registered zone (not wired)");
            zones.insert(zone_id.clone(), false);
        }
        info!("[mock] valve board initialised (no hardware)");
        Ok(Self { zones })
    }

    pub(crate) fn set(&mut self, zone_id: &str, on: bool) {
        if let Some(state) = self.zones.get_mut(zone_id) {
            *state = on;
            info!(
                zone = %zone_id,
                state = if on { "ON" } else { "OFF" },
                "[mock] valve set"
            );
        } else {
            warn!(zone = %zone_id, "[mock] unknown zone_id");
        }
    }

    pub(crate) fn all_off(&mut self) {
        let keys: Vec<String> = self.zones.keys().cloned().collect();
        for k in keys {
            self.set(&k, false);
        }
    }
}

#[cfg(not(feature = "gpio"))]
impl Drop for ValveBoard {
    fn drop(&mut self) {
        self.all_off();
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

    #[test]
    fn valve_board_drop_turns_off() {
        let zones = vec![("z1".to_string(), 17)];
        let mut board = ValveBoard::new(&zones, true).unwrap();
        board.set("z1", true);
        assert!(board.zones["z1"]);
        drop(board);
        // Can't check state after drop, but at least it doesn't panic
    }
}
