import { useEffect, useState } from "preact/hooks";

import { fetchCounters } from "@/api";
import { Badge } from "@/components/ui/badge";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { useSensors, useStatus, useZones } from "@/hooks/use-api";
import type {
  DailyCounters,
  SensorConfig,
  SensorReading,
  ZoneConfig,
  ZoneState,
} from "@/types";

// ── Helpers ──────────────────────────────────────────────────────

function today(): string {
  return new Date().toISOString().slice(0, 10);
}

function formatDuration(secs: number): string {
  const m = Math.floor(secs / 60);
  const s = Math.round(secs % 60);
  return `${m}m ${s}s`;
}

function timeAgo(iso: string): string {
  const diff = Date.now() - new Date(iso).getTime();
  if (diff < 0) return "just now";
  const secs = Math.floor(diff / 1000);
  if (secs < 60) return `${secs}s ago`;
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  const days = Math.floor(hrs / 24);
  return `${days}d ago`;
}

function clamp(v: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, v));
}

function computeMoisture(raw: number, sensor: SensorConfig): number {
  if (sensor.raw_dry === sensor.raw_wet) return 0;
  return clamp(
    (sensor.raw_dry - raw) / (sensor.raw_dry - sensor.raw_wet),
    0,
    1,
  );
}

function moistureBarColor(
  pct: number,
  minMoisture: number,
  targetMoisture: number,
): string {
  if (pct < minMoisture) return "bg-red-500";
  if (pct < targetMoisture) return "bg-amber-500";
  return "bg-green-500";
}

function usageBarColor(ratio: number): string {
  if (ratio >= 0.9) return "bg-red-500";
  if (ratio >= 0.7) return "bg-amber-500";
  return "bg-blue-500";
}

// ── Progress Bar ─────────────────────────────────────────────────

function ProgressBar({
  value,
  max,
  colorClass,
  label,
}: {
  value: number;
  max: number;
  colorClass: string;
  label: string;
}) {
  const pct = max > 0 ? clamp((value / max) * 100, 0, 100) : 0;
  return (
    <div className="space-y-1">
      <div className="flex justify-between text-xs text-muted-foreground">
        <span>{label}</span>
        <span>{Math.round(pct)}%</span>
      </div>
      <div className="h-2 w-full overflow-hidden rounded-full bg-secondary">
        <div
          className={`h-full rounded-full transition-all ${colorClass}`}
          style={{ width: `${pct}%` }}
        />
      </div>
    </div>
  );
}

// ── Zone Card ────────────────────────────────────────────────────

function ZoneCard({
  zone,
  state,
  sensors,
  allReadings,
  counters,
  countersLoading,
  mode,
}: {
  zone: ZoneConfig;
  state: ZoneState | undefined;
  sensors: SensorConfig[];
  allReadings: Record<string, SensorReading[]>;
  counters: DailyCounters | null;
  countersLoading: boolean;
  mode?: "auto" | "monitor";
}) {
  const isOn = state?.on ?? false;
  const isMonitor = mode === "monitor";

  // Find the latest moisture reading for sensors belonging to this zone
  const zoneSensors = sensors.filter((s) => s.zone_id === zone.zone_id);
  let latestMoisture: number | null = null;
  for (const sensor of zoneSensors) {
    for (const readings of Object.values(allReadings)) {
      const reading = readings.find((r) => r.sensor_id === sensor.sensor_id);
      if (reading) {
        latestMoisture = computeMoisture(reading.raw, sensor);
        break;
      }
    }
    if (latestMoisture !== null) break;
  }

  const isBelowMin = latestMoisture !== null && latestMoisture < zone.min_moisture;

  const openSecRatio = counters
    ? counters.open_sec / zone.max_open_sec_per_day
    : 0;
  const pulsesRatio = counters ? counters.pulses / zone.max_pulses_per_day : 0;

  return (
    <Card className="@container/card">
      <CardHeader>
        <div className="flex items-center justify-between">
          <CardTitle className="text-base font-semibold">{zone.name}</CardTitle>
          {isMonitor ? (
            latestMoisture !== null && (
              <Badge
                variant="outline"
                className={
                  isBelowMin
                    ? "border-red-500 bg-red-50 text-red-700 dark:bg-red-950 dark:text-red-400"
                    : "border-green-500 bg-green-50 text-green-700 dark:bg-green-950 dark:text-green-400"
                }
              >
                {isBelowMin ? "Low Moisture" : "OK"}
              </Badge>
            )
          ) : (
            <Badge
              variant="outline"
              className={
                isOn
                  ? "border-green-500 bg-green-50 text-green-700 dark:bg-green-950 dark:text-green-400"
                  : "border-muted text-muted-foreground"
              }
            >
              {isOn && (
                <span className="relative mr-1 flex size-2">
                  <span className="absolute inline-flex size-full animate-ping rounded-full bg-green-400 opacity-75" />
                  <span className="relative inline-flex size-2 rounded-full bg-green-500" />
                </span>
              )}
              {isOn ? "ON" : "OFF"}
            </Badge>
          )}
        </div>
        {!isMonitor && state?.last_changed && (
          <CardDescription>
            Valve changed {timeAgo(state.last_changed)}
          </CardDescription>
        )}
      </CardHeader>

      <CardContent className="space-y-3">
        {/* Moisture */}
        {latestMoisture !== null ? (
          <ProgressBar
            value={latestMoisture * 100}
            max={100}
            colorClass={moistureBarColor(
              latestMoisture,
              zone.min_moisture,
              zone.target_moisture,
            )}
            label="Moisture"
          />
        ) : (
          <div className="space-y-1">
            <span className="text-xs text-muted-foreground">Moisture</span>
            <div className="text-xs italic text-muted-foreground">
              No sensor data
            </div>
          </div>
        )}

        {/* Daily Usage — auto mode only */}
        {!isMonitor && (
          countersLoading ? (
            <div className="space-y-2">
              <Skeleton className="h-2 w-full" />
              <Skeleton className="h-2 w-full" />
            </div>
          ) : (
            <>
              <ProgressBar
                value={counters?.open_sec ?? 0}
                max={zone.max_open_sec_per_day}
                colorClass={usageBarColor(openSecRatio)}
                label={`Daily usage · ${formatDuration(counters?.open_sec ?? 0)} / ${formatDuration(zone.max_open_sec_per_day)}`}
              />
              <ProgressBar
                value={counters?.pulses ?? 0}
                max={zone.max_pulses_per_day}
                colorClass={usageBarColor(pulsesRatio)}
                label={`Pulses · ${counters?.pulses ?? 0} / ${zone.max_pulses_per_day}`}
              />
            </>
          )
        )}
      </CardContent>
    </Card>
  );
}

// ── Zone Cards Grid ──────────────────────────────────────────────

export function ZoneCards() {
  const { data: status, loading: statusLoading } = useStatus();
  const { data: zones, loading: zonesLoading } = useZones();
  const { data: sensors } = useSensors();

  // Fetch counters for all zones in parallel
  const [countersMap, setCountersMap] = useState<Record<string, DailyCounters>>(
    {},
  );
  const [countersLoading, setCountersLoading] = useState(true);

  useEffect(() => {
    if (!zones || zones.length === 0) return;
    let cancelled = false;
    const day = today();

    const doFetch = () => {
      Promise.all(
        zones.map((z) =>
          fetchCounters(z.zone_id, day)
            .then((c) => [z.zone_id, c] as const)
            .catch(() => null),
        ),
      ).then((results) => {
        if (cancelled) return;
        const map: Record<string, DailyCounters> = {};
        for (const r of results) {
          if (r) map[r[0]] = r[1];
        }
        setCountersMap(map);
        setCountersLoading(false);
      });
    };

    doFetch();
    const id = setInterval(doFetch, 30_000);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [zones]);

  // Build a flat map: nodeId -> readings[] from live status
  const allReadings: Record<string, SensorReading[]> = status?.nodes
    ? Object.fromEntries(
        Object.entries(status.nodes).map(([id, n]) => [id, n.readings]),
      )
    : {};

  if (statusLoading || zonesLoading) {
    return (
      <div className="grid grid-cols-1 gap-4 px-4 lg:px-6 @xl/main:grid-cols-2 @5xl/main:grid-cols-3">
        {Array.from({ length: 3 }).map((_, i) => (
          <Card key={i} className="@container/card">
            <CardHeader>
              <Skeleton className="h-5 w-32" />
              <Skeleton className="h-4 w-20" />
            </CardHeader>
            <CardContent className="space-y-3">
              <Skeleton className="h-2 w-full" />
              <Skeleton className="h-2 w-full" />
              <Skeleton className="h-2 w-full" />
            </CardContent>
          </Card>
        ))}
      </div>
    );
  }

  if (!zones || zones.length === 0) {
    return (
      <div className="px-4 lg:px-6">
        <p className="text-sm text-muted-foreground">No zones configured.</p>
      </div>
    );
  }

  return (
    <div className="grid grid-cols-1 gap-4 px-4 lg:px-6 @xl/main:grid-cols-2 @5xl/main:grid-cols-3">
      {zones.map((zone) => (
        <ZoneCard
          key={zone.zone_id}
          zone={zone}
          state={status?.zones[zone.zone_id]}
          sensors={sensors ?? []}
          allReadings={allReadings}
          counters={countersMap[zone.zone_id] ?? null}
          countersLoading={countersLoading}
          mode={status?.mode}
        />
      ))}
    </div>
  );
}
