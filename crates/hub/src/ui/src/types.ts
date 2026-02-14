// ── Status endpoint ─────────────────────────────────────────────

export interface StatusResponse {
  uptime_secs: number;
  mqtt_connected: boolean;
  nodes: Record<string, NodeState>;
  zones: Record<string, ZoneState>;
  events: SystemEvent[];
}

export interface NodeState {
  /** ISO-8601 timestamp */
  last_seen: string;
  readings: SensorReading[];
}

export interface SensorReading {
  sensor_id: string;
  raw: number;
}

export interface ZoneState {
  on: boolean;
  gpio_pin: number;
  /** ISO-8601 timestamp, null if never toggled */
  last_changed: string | null;
}

export type EventKind = "reading" | "valve" | "error" | "system";

export interface SystemEvent {
  /** ISO-8601 timestamp */
  ts: string;
  kind: EventKind;
  detail: string;
}

// ── Zone config ─────────────────────────────────────────────────

export interface ZoneConfig {
  zone_id: string;
  name: string;
  min_moisture: number;
  target_moisture: number;
  pulse_sec: number;
  soak_min: number;
  max_open_sec_per_day: number;
  max_pulses_per_day: number;
  stale_timeout_min: number;
  valve_gpio_pin: number;
}

// ── Sensor config ───────────────────────────────────────────────

export interface SensorConfig {
  sensor_id: string;
  node_id: string;
  zone_id: string;
  raw_dry: number;
  raw_wet: number;
}

// ── Readings ────────────────────────────────────────────────────

export interface ReadingRow {
  /** Unix epoch seconds */
  ts: number;
  sensor_id: string;
  raw: number;
  /** 0.0 – 1.0 */
  moisture: number;
}

export interface ReadingsParams {
  sensor_id?: string;
  zone_id?: string;
  limit?: number;
  offset?: number;
}

// ── Watering events ─────────────────────────────────────────────

export interface WateringEventRow {
  /** Unix epoch seconds */
  ts_start: number;
  /** Unix epoch seconds */
  ts_end: number;
  zone_id: string;
  reason: string;
  result: string;
}

export interface WateringEventsParams {
  zone_id?: string;
  limit?: number;
  offset?: number;
}

// ── Daily counters ──────────────────────────────────────────────

export interface DailyCounters {
  /** YYYY-MM-DD */
  day: string;
  zone_id: string;
  open_sec: number;
  pulses: number;
}
