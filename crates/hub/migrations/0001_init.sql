CREATE TABLE IF NOT EXISTS zones (
  zone_id TEXT PRIMARY KEY,
  name TEXT NOT NULL,

  min_moisture REAL NOT NULL,
  target_moisture REAL NOT NULL,

  pulse_sec INTEGER NOT NULL,
  soak_min INTEGER NOT NULL,

  max_open_sec_per_day INTEGER NOT NULL,
  max_pulses_per_day INTEGER NOT NULL,
  stale_timeout_min INTEGER NOT NULL,

  valve_gpio_pin INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sensors (
  sensor_id TEXT PRIMARY KEY,
  node_id TEXT NOT NULL,
  zone_id TEXT NOT NULL,

  -- Calibration (raw -> moisture 0..1)
  raw_dry INTEGER NOT NULL,
  raw_wet INTEGER NOT NULL,

  FOREIGN KEY(zone_id) REFERENCES zones(zone_id)
);

CREATE TABLE IF NOT EXISTS readings (
  ts INTEGER NOT NULL,          -- unix seconds
  sensor_id TEXT NOT NULL,
  raw INTEGER NOT NULL,
  moisture REAL NOT NULL,

  PRIMARY KEY (ts, sensor_id),
  FOREIGN KEY(sensor_id) REFERENCES sensors(sensor_id)
);

CREATE INDEX IF NOT EXISTS idx_readings_sensor_ts ON readings(sensor_id, ts);

CREATE TABLE IF NOT EXISTS watering_events (
  ts_start INTEGER NOT NULL,
  ts_end INTEGER NOT NULL,
  zone_id TEXT NOT NULL,
  reason TEXT NOT NULL,
  result TEXT NOT NULL,

  FOREIGN KEY(zone_id) REFERENCES zones(zone_id)
);

CREATE INDEX IF NOT EXISTS idx_watering_events_zone_ts ON watering_events(zone_id, ts_start);

CREATE TABLE IF NOT EXISTS zone_daily_counters (
  day TEXT NOT NULL,            -- "YYYY-MM-DD"
  zone_id TEXT NOT NULL,

  open_sec INTEGER NOT NULL,
  pulses INTEGER NOT NULL,

  PRIMARY KEY (day, zone_id),
  FOREIGN KEY(zone_id) REFERENCES zones(zone_id)
);