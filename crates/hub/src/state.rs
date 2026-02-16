//! In-memory system state for the live web dashboard: node telemetry, zone
//! valve status, and a capped event ring buffer.

use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;
use time::OffsetDateTime;
use tokio::sync::RwLock;

/// Maximum number of events retained in the ring buffer.
const MAX_EVENTS: usize = 200;

// ---------------------------------------------------------------------------
// Public type alias
// ---------------------------------------------------------------------------

pub type SharedState = Arc<RwLock<SystemState>>;

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

pub struct SystemState {
    pub started_at: Instant,
    pub mqtt_connected: bool,
    /// Operation mode label: "auto" or "monitor".
    pub mode: String,
    pub nodes: HashMap<String, NodeState>,
    pub zones: HashMap<String, ZoneState>,
    pub events: VecDeque<SystemEvent>,
    /// CPU usage as a percentage (0.0 - 100.0).
    pub cpu_usage_percent: f32,
    /// Memory currently used in bytes.
    pub memory_used_bytes: u64,
    /// Total system memory in bytes.
    pub memory_total_bytes: u64,
}

#[derive(Clone, Serialize)]
pub struct NodeState {
    #[serde(with = "time::serde::rfc3339")]
    pub last_seen: OffsetDateTime,
    /// Whether the node is connected to MQTT (tracked via LWT status messages).
    pub online: bool,
    pub readings: Vec<SensorReading>,
}

#[derive(Clone, Serialize)]
pub struct SensorReading {
    pub sensor_id: String,
    pub raw: i64,
}

#[derive(Clone, Serialize)]
pub struct ZoneState {
    pub on: bool,
    pub gpio_pin: u8,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_changed: Option<OffsetDateTime>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SystemEvent {
    #[serde(with = "time::serde::rfc3339")]
    pub ts: OffsetDateTime,
    pub kind: EventKind,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EventKind {
    Reading,
    Valve,
    Error,
    System,
    Scheduler,
}

// ---------------------------------------------------------------------------
// JSON response (what the API returns)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct StatusResponse {
    pub uptime_secs: u64,
    pub mqtt_connected: bool,
    pub mode: String,
    pub nodes: HashMap<String, NodeState>,
    pub zones: HashMap<String, ZoneState>,
    pub events: Vec<SystemEvent>,
    pub cpu_usage_percent: f32,
    pub memory_used_bytes: u64,
    pub memory_total_bytes: u64,
}

// ---------------------------------------------------------------------------
// Construction & mutation
// ---------------------------------------------------------------------------

impl SystemState {
    pub fn new(zone_to_gpio: &[(String, u8)], mode: &str) -> Self {
        let mut zones = HashMap::new();
        for (zone_id, pin) in zone_to_gpio {
            zones.insert(
                zone_id.clone(),
                ZoneState {
                    on: false,
                    gpio_pin: *pin,
                    last_changed: None,
                },
            );
        }

        Self {
            started_at: Instant::now(),
            mqtt_connected: false,
            mode: mode.to_string(),
            nodes: HashMap::new(),
            zones,
            events: VecDeque::with_capacity(MAX_EVENTS),
            cpu_usage_percent: 0.0,
            memory_used_bytes: 0,
            memory_total_bytes: 0,
        }
    }

    /// Record a telemetry reading from a node.
    pub fn record_reading(&mut self, node_id: &str, readings: Vec<SensorReading>) {
        let now = OffsetDateTime::now_utc();

        let detail = format!(
            "{node_id}: {}",
            readings
                .iter()
                .map(|r| format!("{}={}", r.sensor_id, r.raw))
                .collect::<Vec<_>>()
                .join(", ")
        );

        self.nodes.insert(
            node_id.to_string(),
            NodeState {
                last_seen: now,
                online: true, // receiving data proves the node is alive
                readings,
            },
        );

        self.push_event(EventKind::Reading, detail);
    }

    /// Record a node's online/offline status from an MQTT LWT or status
    /// announcement on `status/node/<node_id>`.
    pub fn record_node_status(&mut self, node_id: &str, online: bool) {
        let now = OffsetDateTime::now_utc();
        let entry = self
            .nodes
            .entry(node_id.to_string())
            .or_insert_with(|| NodeState {
                last_seen: now,
                online: false,
                readings: Vec::new(),
            });
        entry.online = online;
        if online {
            entry.last_seen = now;
        }

        let status_str = if online { "online" } else { "offline" };
        self.push_event(EventKind::System, format!("node {node_id} {status_str}"));
    }

    /// Record a valve state change.
    pub fn record_valve(&mut self, zone_id: &str, on: bool) {
        if let Some(zone) = self.zones.get_mut(zone_id) {
            zone.on = on;
            zone.last_changed = Some(OffsetDateTime::now_utc());
        }

        let state_str = if on { "ON" } else { "OFF" };
        self.push_event(EventKind::Valve, format!("{zone_id} set {state_str}"));
    }

    /// Record an error event.
    pub fn record_error(&mut self, detail: String) {
        self.push_event(EventKind::Error, detail);
    }

    /// Record a generic system event.
    pub fn record_system(&mut self, detail: String) {
        self.push_event(EventKind::System, detail);
    }

    /// Record a scheduler decision event.
    pub fn record_scheduler(&mut self, detail: String) {
        self.push_event(EventKind::Scheduler, detail);
    }

    /// Force all zone states to OFF (used during emergency shutdowns / MQTT errors).
    pub fn set_all_zones_off(&mut self) {
        let now = OffsetDateTime::now_utc();
        for zone in self.zones.values_mut() {
            if zone.on {
                zone.on = false;
                zone.last_changed = Some(now);
            }
        }
    }

    /// Update system resource metrics (CPU, memory).
    pub fn update_system_metrics(&mut self, cpu: f32, mem_used: u64, mem_total: u64) {
        self.cpu_usage_percent = cpu;
        self.memory_used_bytes = mem_used;
        self.memory_total_bytes = mem_total;
    }

    /// Build the JSON-serialisable status snapshot.
    pub fn to_status(&self) -> StatusResponse {
        StatusResponse {
            uptime_secs: self.started_at.elapsed().as_secs(),
            mqtt_connected: self.mqtt_connected,
            mode: self.mode.clone(),
            nodes: self.nodes.clone(),
            zones: self.zones.clone(),
            events: self.events.iter().rev().cloned().collect(),
            cpu_usage_percent: self.cpu_usage_percent,
            memory_used_bytes: self.memory_used_bytes,
            memory_total_bytes: self.memory_total_bytes,
        }
    }

    fn push_event(&mut self, kind: EventKind, detail: String) {
        if self.events.len() >= MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(SystemEvent {
            ts: OffsetDateTime::now_utc(),
            kind,
            detail,
        });
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a two-zone state for most tests.
    fn two_zone_state() -> SystemState {
        SystemState::new(
            &[("zone1".to_string(), 17), ("zone2".to_string(), 27)],
            "auto",
        )
    }

    /// Helper: build a simple sensor reading vec.
    fn sample_readings() -> Vec<SensorReading> {
        vec![
            SensorReading {
                sensor_id: "s1".to_string(),
                raw: 20000,
            },
            SensorReading {
                sensor_id: "s2".to_string(),
                raw: 21000,
            },
        ]
    }

    // -- Constructor --------------------------------------------------------

    #[test]
    fn new_creates_zones_with_correct_gpio_pins() {
        let st = two_zone_state();
        assert_eq!(st.zones.len(), 2);
        assert_eq!(st.zones["zone1"].gpio_pin, 17);
        assert_eq!(st.zones["zone2"].gpio_pin, 27);
    }

    #[test]
    fn new_zones_start_off() {
        let st = two_zone_state();
        for zone in st.zones.values() {
            assert!(!zone.on);
            assert!(zone.last_changed.is_none());
        }
    }

    #[test]
    fn new_starts_with_no_nodes() {
        let st = two_zone_state();
        assert!(st.nodes.is_empty());
    }

    #[test]
    fn new_starts_with_empty_events() {
        let st = two_zone_state();
        assert!(st.events.is_empty());
    }

    #[test]
    fn new_mqtt_disconnected_by_default() {
        let st = two_zone_state();
        assert!(!st.mqtt_connected);
    }

    #[test]
    fn new_with_no_zones() {
        let st = SystemState::new(&[], "auto");
        assert!(st.zones.is_empty());
    }

    // -- record_reading -----------------------------------------------------

    #[test]
    fn record_reading_inserts_new_node() {
        let mut st = two_zone_state();
        st.record_reading("node-a", sample_readings());

        assert!(st.nodes.contains_key("node-a"));
        assert_eq!(st.nodes["node-a"].readings.len(), 2);
        assert!(st.nodes["node-a"].online);
    }

    #[test]
    fn record_reading_overwrites_existing_node() {
        let mut st = two_zone_state();
        st.record_reading("node-a", sample_readings());
        let first_seen = st.nodes["node-a"].last_seen;

        // Overwrite with a different reading
        let new = vec![SensorReading {
            sensor_id: "s3".to_string(),
            raw: 99,
        }];
        st.record_reading("node-a", new);

        assert_eq!(st.nodes["node-a"].readings.len(), 1);
        assert_eq!(st.nodes["node-a"].readings[0].sensor_id, "s3");
        assert!(st.nodes["node-a"].last_seen >= first_seen);
    }

    #[test]
    fn record_reading_creates_event() {
        let mut st = two_zone_state();
        st.record_reading("node-a", sample_readings());

        assert_eq!(st.events.len(), 1);
        assert!(matches!(st.events[0].kind, EventKind::Reading));
    }

    #[test]
    fn record_reading_event_detail_format() {
        let mut st = two_zone_state();
        st.record_reading("node-a", sample_readings());

        assert_eq!(st.events[0].detail, "node-a: s1=20000, s2=21000");
    }

    #[test]
    fn record_reading_multiple_nodes() {
        let mut st = two_zone_state();
        st.record_reading("node-a", sample_readings());
        st.record_reading("node-b", sample_readings());

        assert_eq!(st.nodes.len(), 2);
        assert!(st.nodes.contains_key("node-a"));
        assert!(st.nodes.contains_key("node-b"));
    }

    // -- record_node_status -------------------------------------------------

    #[test]
    fn record_node_status_online_creates_entry() {
        let mut st = two_zone_state();
        st.record_node_status("node-a", true);

        assert!(st.nodes.contains_key("node-a"));
        assert!(st.nodes["node-a"].online);
        assert!(st.nodes["node-a"].readings.is_empty());
    }

    #[test]
    fn record_node_status_offline_creates_entry() {
        let mut st = two_zone_state();
        st.record_node_status("node-a", false);

        assert!(st.nodes.contains_key("node-a"));
        assert!(!st.nodes["node-a"].online);
    }

    #[test]
    fn record_node_status_offline_preserves_readings() {
        let mut st = two_zone_state();
        st.record_reading("node-a", sample_readings());
        assert_eq!(st.nodes["node-a"].readings.len(), 2);

        st.record_node_status("node-a", false);
        assert!(!st.nodes["node-a"].online);
        // Readings should still be there — only online flag changed
        assert_eq!(st.nodes["node-a"].readings.len(), 2);
    }

    #[test]
    fn record_node_status_updates_last_seen_on_online() {
        let mut st = two_zone_state();
        st.record_node_status("node-a", false);
        let first_seen = st.nodes["node-a"].last_seen;

        st.record_node_status("node-a", true);
        assert!(st.nodes["node-a"].last_seen >= first_seen);
    }

    #[test]
    fn record_node_status_does_not_update_last_seen_on_offline() {
        let mut st = two_zone_state();
        st.record_node_status("node-a", true);
        let seen_when_online = st.nodes["node-a"].last_seen;

        st.record_node_status("node-a", false);
        // last_seen should not have been updated for offline
        assert_eq!(st.nodes["node-a"].last_seen, seen_when_online);
    }

    #[test]
    fn record_node_status_creates_system_event() {
        let mut st = two_zone_state();
        st.record_node_status("node-a", true);

        assert_eq!(st.events.len(), 1);
        assert!(matches!(st.events[0].kind, EventKind::System));
        assert_eq!(st.events[0].detail, "node node-a online");
    }

    #[test]
    fn record_node_status_offline_event_detail() {
        let mut st = two_zone_state();
        st.record_node_status("node-a", false);

        assert_eq!(st.events[0].detail, "node node-a offline");
    }

    // -- record_valve -------------------------------------------------------

    #[test]
    fn record_valve_turns_zone_on() {
        let mut st = two_zone_state();
        st.record_valve("zone1", true);

        assert!(st.zones["zone1"].on);
        assert!(!st.zones["zone2"].on); // untouched
    }

    #[test]
    fn record_valve_turns_zone_off() {
        let mut st = two_zone_state();
        st.record_valve("zone1", true);
        st.record_valve("zone1", false);

        assert!(!st.zones["zone1"].on);
    }

    #[test]
    fn record_valve_sets_last_changed() {
        let mut st = two_zone_state();
        assert!(st.zones["zone1"].last_changed.is_none());

        st.record_valve("zone1", true);
        assert!(st.zones["zone1"].last_changed.is_some());
    }

    #[test]
    fn record_valve_unknown_zone_does_not_panic() {
        let mut st = two_zone_state();
        // Should not panic — just logs the event
        st.record_valve("nonexistent", true);

        // Event is still recorded even for unknown zones
        assert_eq!(st.events.len(), 1);
        assert!(matches!(st.events[0].kind, EventKind::Valve));
    }

    #[test]
    fn record_valve_event_detail_on() {
        let mut st = two_zone_state();
        st.record_valve("zone1", true);
        assert_eq!(st.events[0].detail, "zone1 set ON");
    }

    #[test]
    fn record_valve_event_detail_off() {
        let mut st = two_zone_state();
        st.record_valve("zone2", false);
        assert_eq!(st.events[0].detail, "zone2 set OFF");
    }

    // -- record_error / record_system ---------------------------------------

    #[test]
    fn record_error_creates_error_event() {
        let mut st = two_zone_state();
        st.record_error("something broke".to_string());

        assert_eq!(st.events.len(), 1);
        assert!(matches!(st.events[0].kind, EventKind::Error));
        assert_eq!(st.events[0].detail, "something broke");
    }

    #[test]
    fn record_system_creates_system_event() {
        let mut st = two_zone_state();
        st.record_system("hub started".to_string());

        assert_eq!(st.events.len(), 1);
        assert!(matches!(st.events[0].kind, EventKind::System));
        assert_eq!(st.events[0].detail, "hub started");
    }

    // -- Ring buffer --------------------------------------------------------

    #[test]
    fn event_ring_buffer_caps_at_max() {
        let mut st = two_zone_state();
        for i in 0..MAX_EVENTS + 50 {
            st.record_system(format!("event {i}"));
        }
        assert_eq!(st.events.len(), MAX_EVENTS);
    }

    #[test]
    fn event_ring_buffer_evicts_oldest() {
        let mut st = two_zone_state();
        for i in 0..MAX_EVENTS + 10 {
            st.record_system(format!("event {i}"));
        }
        // The oldest remaining event should be event 10 (0..9 were evicted)
        assert_eq!(st.events.front().unwrap().detail, "event 10");
        assert_eq!(
            st.events.back().unwrap().detail,
            format!("event {}", MAX_EVENTS + 9)
        );
    }

    // -- to_status ----------------------------------------------------------

    // -- set_all_zones_off ---------------------------------------------------

    #[test]
    fn set_all_zones_off_turns_all_zones_off() {
        let mut st = two_zone_state();
        st.record_valve("zone1", true);
        st.record_valve("zone2", true);
        assert!(st.zones["zone1"].on);
        assert!(st.zones["zone2"].on);

        st.set_all_zones_off();
        assert!(!st.zones["zone1"].on);
        assert!(!st.zones["zone2"].on);
    }

    #[test]
    fn set_all_zones_off_sets_last_changed() {
        let mut st = two_zone_state();
        st.record_valve("zone1", true);
        st.set_all_zones_off();
        assert!(st.zones["zone1"].last_changed.is_some());
    }

    #[test]
    fn set_all_zones_off_noop_when_already_off() {
        let mut st = two_zone_state();
        // Zones start off, last_changed is None
        st.set_all_zones_off();
        // last_changed should still be None since zones were already off
        assert!(st.zones["zone1"].last_changed.is_none());
        assert!(st.zones["zone2"].last_changed.is_none());
    }

    // -- to_status ----------------------------------------------------------

    #[test]
    fn to_status_returns_events_in_reverse_order() {
        let mut st = two_zone_state();
        st.record_system("first".to_string());
        st.record_system("second".to_string());
        st.record_system("third".to_string());

        let status = st.to_status();
        assert_eq!(status.events[0].detail, "third");
        assert_eq!(status.events[1].detail, "second");
        assert_eq!(status.events[2].detail, "first");
    }

    #[test]
    fn to_status_reflects_mqtt_connected() {
        let mut st = two_zone_state();
        assert!(!st.to_status().mqtt_connected);

        st.mqtt_connected = true;
        assert!(st.to_status().mqtt_connected);
    }

    #[test]
    fn to_status_uptime_is_non_negative() {
        let st = two_zone_state();
        // uptime should be 0 or very small since we just created it
        assert!(st.to_status().uptime_secs < 2);
    }

    #[test]
    fn to_status_includes_zones_and_nodes() {
        let mut st = two_zone_state();
        st.record_reading("node-x", sample_readings());

        let status = st.to_status();
        assert_eq!(status.zones.len(), 2);
        assert_eq!(status.nodes.len(), 1);
        assert!(status.nodes.contains_key("node-x"));
    }

    #[test]
    fn to_status_includes_mode() {
        let st = SystemState::new(&[("z1".to_string(), 17)], "monitor");
        let status = st.to_status();
        assert_eq!(status.mode, "monitor");

        let st_auto = two_zone_state();
        assert_eq!(st_auto.to_status().mode, "auto");
    }

    #[test]
    fn to_status_serializes_to_json() {
        let mut st = two_zone_state();
        st.record_reading("node-a", sample_readings());
        st.record_valve("zone1", true);

        let status = st.to_status();
        let json = serde_json::to_value(&status).expect("should serialize");

        assert!(json["uptime_secs"].is_u64());
        assert!(json["mqtt_connected"].is_boolean());
        assert!(json["nodes"].is_object());
        assert!(json["zones"].is_object());
        assert!(json["events"].is_array());
    }
}
