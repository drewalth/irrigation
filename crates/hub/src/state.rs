use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Instant;
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
    pub nodes: HashMap<String, NodeState>,
    pub zones: HashMap<String, ZoneState>,
    pub events: VecDeque<SystemEvent>,
}

#[derive(Clone, Serialize)]
pub struct NodeState {
    pub last_seen: DateTime<Utc>,
    pub readings: Vec<SensorReading>,
}

#[derive(Clone, Serialize)]
pub struct SensorReading {
    pub sensor_id: String,
    pub raw: i32,
}

#[derive(Clone, Serialize)]
pub struct ZoneState {
    pub on: bool,
    pub gpio_pin: u8,
    pub last_changed: Option<DateTime<Utc>>,
}

#[derive(Clone, Serialize)]
pub struct SystemEvent {
    pub ts: DateTime<Utc>,
    pub kind: EventKind,
    pub detail: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EventKind {
    Reading,
    Valve,
    Error,
    System,
}

// ---------------------------------------------------------------------------
// JSON response (what the API returns)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct StatusResponse {
    pub uptime_secs: u64,
    pub mqtt_connected: bool,
    pub nodes: HashMap<String, NodeState>,
    pub zones: HashMap<String, ZoneState>,
    pub events: Vec<SystemEvent>,
}

// ---------------------------------------------------------------------------
// Construction & mutation
// ---------------------------------------------------------------------------

impl SystemState {
    pub fn new(zone_to_gpio: &[(String, u8)]) -> Self {
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
            nodes: HashMap::new(),
            zones,
            events: VecDeque::with_capacity(MAX_EVENTS),
        }
    }

    /// Record a telemetry reading from a node.
    pub fn record_reading(&mut self, node_id: &str, readings: Vec<SensorReading>) {
        let now = Utc::now();

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
                readings,
            },
        );

        self.push_event(EventKind::Reading, detail);
    }

    /// Record a valve state change.
    pub fn record_valve(&mut self, zone_id: &str, on: bool) {
        if let Some(zone) = self.zones.get_mut(zone_id) {
            zone.on = on;
            zone.last_changed = Some(Utc::now());
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

    /// Build the JSON-serialisable status snapshot.
    pub fn to_status(&self) -> StatusResponse {
        StatusResponse {
            uptime_secs: self.started_at.elapsed().as_secs(),
            mqtt_connected: self.mqtt_connected,
            nodes: self.nodes.clone(),
            zones: self.zones.clone(),
            events: self.events.iter().rev().cloned().collect(),
        }
    }

    fn push_event(&mut self, kind: EventKind, detail: String) {
        if self.events.len() >= MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(SystemEvent {
            ts: Utc::now(),
            kind,
            detail,
        });
    }
}
