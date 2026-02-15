import { useEffect, useState } from "preact/hooks";

import { fetchCounters } from "@/api";
import { Badge } from "@/components/ui/badge";
import {
  Card,
  CardAction,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { useSensors, useStatus, useZones } from "@/hooks/use-api";
import type { DailyCounters } from "@/types";

// ── Helpers ──────────────────────────────────────────────────────

function formatUptime(secs: number): string {
  const d = Math.floor(secs / 86400);
  const h = Math.floor((secs % 86400) / 3600);
  const m = Math.floor((secs % 3600) / 60);
  return `${d}d ${h}h ${m}m`;
}

function formatDuration(secs: number): string {
  const m = Math.floor(secs / 60);
  const s = Math.round(secs % 60);
  return `${m}m ${s}s`;
}

function today(): string {
  return new Date().toISOString().slice(0, 10);
}

// ── Section Cards ────────────────────────────────────────────────

export function SectionCards() {
  const { data: status, loading: statusLoading } = useStatus();
  const { data: zones, loading: zonesLoading } = useZones();
  const { data: sensors } = useSensors();

  // Fetch counters for all zones in parallel
  const [counters, setCounters] = useState<DailyCounters[]>([]);
  const [countersLoading, setCountersLoading] = useState(true);

  useEffect(() => {
    if (!zones || zones.length === 0) return;
    let cancelled = false;
    const day = today();

    const doFetch = () => {
      Promise.all(
        zones.map((z) => fetchCounters(z.zone_id, day).catch(() => null)),
      ).then((results) => {
        if (cancelled) return;
        setCounters(results.filter((r): r is DailyCounters => r !== null));
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

  // ── Derived values ───────────────────────────────────────────

  const zoneEntries = status ? Object.entries(status.zones) : [];
  const activeZones = zoneEntries.filter(([, z]) => z.on);
  const totalZones = zones?.length ?? zoneEntries.length;

  const nodeEntries = status ? Object.entries(status.nodes) : [];
  const staleThresholdMs = 10 * 60 * 1000;
  const now = Date.now();
  const staleNodes = nodeEntries.filter(
    ([, n]) => now - new Date(n.last_seen).getTime() > staleThresholdMs,
  );

  const totalOpenSec = counters.reduce((sum, c) => sum + c.open_sec, 0);
  const totalPulses = counters.reduce((sum, c) => sum + c.pulses, 0);

  const mqttOk = status?.mqtt_connected ?? false;
  const isHealthy = mqttOk;

  const zoneNameMap = new Map(zones?.map((z) => [z.zone_id, z.name]) ?? []);

  const isMonitor = status?.mode === "monitor";

  // Count zones with moisture below min (for monitor mode).
  const alertZoneIds: string[] = [];
  if (isMonitor && zones && sensors && status) {
    for (const zone of zones) {
      const zoneSensors = sensors.filter((s) => s.zone_id === zone.zone_id);
      let moisture: number | null = null;
      for (const sensor of zoneSensors) {
        for (const readings of Object.values(status.nodes)) {
          const reading = readings.readings.find(
            (r) => r.sensor_id === sensor.sensor_id,
          );
          if (reading) {
            const raw_range = sensor.raw_dry - sensor.raw_wet;
            moisture =
              raw_range !== 0
                ? Math.max(0, Math.min(1, (sensor.raw_dry - reading.raw) / raw_range))
                : 0;
            break;
          }
        }
        if (moisture !== null) break;
      }
      if (moisture !== null && moisture < zone.min_moisture) {
        alertZoneIds.push(zone.zone_id);
      }
    }
  }

  return (
    <div className="*:data-[slot=card]:from-primary/5 *:data-[slot=card]:to-card dark:*:data-[slot=card]:bg-card grid grid-cols-1 gap-4 px-4 *:data-[slot=card]:bg-gradient-to-t *:data-[slot=card]:shadow-xs lg:px-6 @xl/main:grid-cols-2 @5xl/main:grid-cols-4">
      {/* ── Card 1: System Health ── */}
      <Card
        className={`@container/card ${
          isHealthy
            ? "border-green-200 dark:border-green-900"
            : "border-red-200 dark:border-red-900"
        }`}
      >
        <CardHeader>
          <CardDescription>System Health</CardDescription>
          {statusLoading ? (
            <Skeleton className="h-8 w-32" />
          ) : (
            <CardTitle className="text-2xl font-semibold tabular-nums @[250px]/card:text-3xl">
              {formatUptime(status!.uptime_secs)}
            </CardTitle>
          )}
          <CardAction>
            {statusLoading ? (
              <Skeleton className="h-5 w-20" />
            ) : (
              <Badge
                variant="outline"
                className={
                  mqttOk
                    ? "border-green-500 bg-green-50 text-green-700 dark:bg-green-950 dark:text-green-400"
                    : "border-red-500 bg-red-50 text-red-700 dark:bg-red-950 dark:text-red-400"
                }
              >
                <span
                  className={`inline-block size-2 rounded-full ${
                    mqttOk ? "bg-green-500" : "bg-red-500"
                  }`}
                />
                {mqttOk ? "Connected" : "Disconnected"}
              </Badge>
            )}
            {!statusLoading && status && (
              <Badge
                variant="outline"
                className={
                  status.mode === "monitor"
                    ? "border-purple-500 bg-purple-50 text-purple-700 dark:bg-purple-950 dark:text-purple-400"
                    : "border-blue-500 bg-blue-50 text-blue-700 dark:bg-blue-950 dark:text-blue-400"
                }
              >
                {status.mode === "monitor" ? "Monitor" : "Auto"}
              </Badge>
            )}
          </CardAction>
        </CardHeader>
        <CardFooter className="flex-col items-start gap-1.5 text-sm">
          {statusLoading ? (
            <Skeleton className="h-4 w-40" />
          ) : (
            <div className="text-muted-foreground">
              MQTT {mqttOk ? "connected" : "disconnected"} · Uptime{" "}
              {formatUptime(status!.uptime_secs)}
            </div>
          )}
        </CardFooter>
      </Card>

      {/* ── Card 2: Active Zones / Moisture Alerts ── */}
      {isMonitor ? (
        <Card className={`@container/card ${alertZoneIds.length > 0 ? "border-red-200 dark:border-red-900" : ""}`}>
          <CardHeader>
            <CardDescription>Moisture Alerts</CardDescription>
            {statusLoading ? (
              <Skeleton className="h-8 w-24" />
            ) : (
              <CardTitle className="text-2xl font-semibold tabular-nums @[250px]/card:text-3xl">
                {alertZoneIds.length} / {totalZones}
              </CardTitle>
            )}
            <CardAction>
              {alertZoneIds.length > 0 && (
                <Badge
                  variant="outline"
                  className="border-red-500 bg-red-50 text-red-700 dark:bg-red-950 dark:text-red-400"
                >
                  Low Moisture
                </Badge>
              )}
              {alertZoneIds.length === 0 && !statusLoading && (
                <Badge
                  variant="outline"
                  className="border-green-500 bg-green-50 text-green-700 dark:bg-green-950 dark:text-green-400"
                >
                  All OK
                </Badge>
              )}
            </CardAction>
          </CardHeader>
          <CardFooter className="flex-col items-start gap-1.5 text-sm">
            {statusLoading ? (
              <Skeleton className="h-4 w-48" />
            ) : alertZoneIds.length > 0 ? (
              <div className="flex flex-wrap gap-2">
                {alertZoneIds.map((id) => (
                  <span
                    key={id}
                    className="inline-flex items-center gap-1.5 text-sm font-medium text-red-600 dark:text-red-400"
                  >
                    <span className="inline-block size-2 rounded-full bg-red-500" />
                    {zoneNameMap.get(id) ?? id}
                  </span>
                ))}
              </div>
            ) : (
              <div className="text-muted-foreground">
                All zones within moisture thresholds
              </div>
            )}
          </CardFooter>
        </Card>
      ) : (
        <Card className="@container/card">
          <CardHeader>
            <CardDescription>Active Zones</CardDescription>
            {statusLoading ? (
              <Skeleton className="h-8 w-24" />
            ) : (
              <CardTitle className="text-2xl font-semibold tabular-nums @[250px]/card:text-3xl">
                {activeZones.length} / {totalZones}
              </CardTitle>
            )}
            <CardAction>
              {activeZones.length > 0 && (
                <Badge
                  variant="outline"
                  className="border-green-500 bg-green-50 text-green-700 dark:bg-green-950 dark:text-green-400"
                >
                  Watering
                </Badge>
              )}
            </CardAction>
          </CardHeader>
          <CardFooter className="flex-col items-start gap-1.5 text-sm">
            {statusLoading ? (
              <Skeleton className="h-4 w-48" />
            ) : activeZones.length > 0 ? (
              <div className="flex flex-wrap gap-2">
                {activeZones.map(([id]) => (
                  <span
                    key={id}
                    className="inline-flex items-center gap-1.5 text-sm font-medium"
                  >
                    <span className="relative flex size-2">
                      <span className="absolute inline-flex size-full animate-ping rounded-full bg-green-400 opacity-75" />
                      <span className="relative inline-flex size-2 rounded-full bg-green-500" />
                    </span>
                    {zoneNameMap.get(id) ?? id}
                  </span>
                ))}
              </div>
            ) : (
              <div className="text-muted-foreground">
                No valves currently open
              </div>
            )}
          </CardFooter>
        </Card>
      )}

      {/* ── Card 3: Node Status ── */}
      <Card className="@container/card">
        <CardHeader>
          <CardDescription>Node Status</CardDescription>
          {statusLoading ? (
            <Skeleton className="h-8 w-28" />
          ) : (
            <CardTitle className="text-2xl font-semibold tabular-nums @[250px]/card:text-3xl">
              {nodeEntries.length} node{nodeEntries.length !== 1 ? "s" : ""}
            </CardTitle>
          )}
          <CardAction>
            {!statusLoading && staleNodes.length > 0 && (
              <Badge
                variant="outline"
                className="border-amber-500 bg-amber-50 text-amber-700 dark:bg-amber-950 dark:text-amber-400"
              >
                {staleNodes.length} stale
              </Badge>
            )}
            {!statusLoading &&
              staleNodes.length === 0 &&
              nodeEntries.length > 0 && (
                <Badge
                  variant="outline"
                  className="border-green-500 bg-green-50 text-green-700 dark:bg-green-950 dark:text-green-400"
                >
                  All OK
                </Badge>
              )}
          </CardAction>
        </CardHeader>
        <CardFooter className="flex-col items-start gap-1.5 text-sm">
          {statusLoading ? (
            <Skeleton className="h-4 w-36" />
          ) : (
            <div className="text-muted-foreground">
              {nodeEntries.length} node{nodeEntries.length !== 1 ? "s" : ""}{" "}
              reporting
              {staleNodes.length > 0 && (
                <span className="text-amber-600 dark:text-amber-400">
                  {" "}
                  · {staleNodes.length} not seen in 10m+
                </span>
              )}
            </div>
          )}
        </CardFooter>
      </Card>

      {/* ── Card 4: Today's Water Usage / Monitoring Summary ── */}
      {!isMonitor && (
        <Card className="@container/card">
          <CardHeader>
            <CardDescription>Today's Water Usage</CardDescription>
            {countersLoading || zonesLoading ? (
              <Skeleton className="h-8 w-28" />
            ) : (
              <CardTitle className="text-2xl font-semibold tabular-nums @[250px]/card:text-3xl">
                {formatDuration(totalOpenSec)}
              </CardTitle>
            )}
            <CardAction>
              {!countersLoading && (
                <Badge variant="outline">
                  {totalPulses} pulse{totalPulses !== 1 ? "s" : ""}
                </Badge>
              )}
            </CardAction>
          </CardHeader>
          <CardFooter className="flex-col items-start gap-1.5 text-sm">
            {countersLoading || zonesLoading ? (
              <Skeleton className="h-4 w-44" />
            ) : (
              <div className="text-muted-foreground">
                {counters.length} zone{counters.length !== 1 ? "s" : ""} ·{" "}
                {totalPulses} pulses · {formatDuration(totalOpenSec)} open
              </div>
            )}
          </CardFooter>
        </Card>
      )}
      {isMonitor && (
        <Card className="@container/card">
          <CardHeader>
            <CardDescription>Monitoring Summary</CardDescription>
            {statusLoading ? (
              <Skeleton className="h-8 w-28" />
            ) : (
              <CardTitle className="text-2xl font-semibold tabular-nums @[250px]/card:text-3xl">
                {totalZones} zone{totalZones !== 1 ? "s" : ""}
              </CardTitle>
            )}
            <CardAction>
              <Badge
                variant="outline"
                className="border-purple-500 bg-purple-50 text-purple-700 dark:bg-purple-950 dark:text-purple-400"
              >
                Monitor
              </Badge>
            </CardAction>
          </CardHeader>
          <CardFooter className="flex-col items-start gap-1.5 text-sm">
            <div className="text-muted-foreground">
              Monitoring only — no valve actuation
            </div>
          </CardFooter>
        </Card>
      )}
    </div>
  );
}
