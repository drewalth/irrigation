import type {
  DailyCounters,
  ReadingRow,
  ReadingsParams,
  SensorConfig,
  StatusResponse,
  WateringEventRow,
  WateringEventsParams,
  ZoneConfig,
} from "./types";

async function get<T>(path: string): Promise<T> {
  const res = await fetch(path);
  if (!res.ok) throw new Error(`GET ${path}: ${res.status}`);
  return res.json();
}

function qs(params: Record<string, string | number | undefined>): string {
  const entries = Object.entries(params).filter(
    ([, v]) => v !== undefined && v !== "",
  );
  if (entries.length === 0) return "";
  return "?" + new URLSearchParams(entries.map(([k, v]) => [k, String(v)])).toString();
}

// ── Endpoints ───────────────────────────────────────────────────

export function fetchStatus(): Promise<StatusResponse> {
  return get("/api/status");
}

export function fetchZones(): Promise<ZoneConfig[]> {
  return get("/api/zones");
}

export function fetchSensors(): Promise<SensorConfig[]> {
  return get("/api/sensors");
}

export function fetchReadings(params: ReadingsParams = {}): Promise<ReadingRow[]> {
  return get(`/api/readings${qs({ ...params })}`);
}

export function fetchWateringEvents(params: WateringEventsParams = {}): Promise<WateringEventRow[]> {
  return get(`/api/watering-events${qs({ ...params })}`);
}

export function fetchCounters(zoneId: string, day?: string): Promise<DailyCounters> {
  return get(`/api/counters/${encodeURIComponent(zoneId)}${qs({ day })}`);
}
