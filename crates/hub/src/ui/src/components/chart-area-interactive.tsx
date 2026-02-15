import { useState, useMemo } from "preact/hooks";
import {
  Area,
  AreaChart,
  CartesianGrid,
  XAxis,
  YAxis,
  ReferenceLine,
} from "recharts";

import { useReadings, useZones } from "@/hooks/use-api";
import { ZoneSelector } from "@/components/zone-selector";
import type { ReadingRow } from "@/types";
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  ChartContainer,
  ChartTooltip,
  type ChartConfig,
} from "@/components/ui/chart";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";

// ── Time range options ──────────────────────────────────────────

interface TimeRangeOption {
  label: string;
  value: string;
  limit: number;
}

const TIME_RANGES: TimeRangeOption[] = [
  { label: "Last 1h", value: "1h", limit: 60 },
  { label: "Last 6h", value: "6h", limit: 360 },
  { label: "Last 24h", value: "24h", limit: 1000 },
  { label: "Last 7d", value: "7d", limit: 1000 },
];

// ── Chart colors per sensor slot ────────────────────────────────

const SENSOR_COLORS = [
  "var(--chart-1)",
  "var(--chart-2)",
  "var(--chart-3)",
  "var(--chart-4)",
  "var(--chart-5)",
];

// ── Helpers ─────────────────────────────────────────────────────

function formatTime(epoch: number): string {
  const d = new Date(epoch * 1000);
  return d.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

function formatTimestamp(epoch: number): string {
  const d = new Date(epoch * 1000);
  return d.toLocaleString([], {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

/**
 * Group flat ReadingRow[] into time-bucketed rows with one moisture key per sensor.
 * Each row: { ts, [sensor_id]: moisture, [`${sensor_id}_raw`]: raw, ... }
 */
function pivotReadings(readings: ReadingRow[]): {
  rows: Record<string, unknown>[];
  sensorIds: string[];
} {
  const sensorSet = new Set<string>();
  const byTs = new Map<number, Record<string, unknown>>();

  for (const r of readings) {
    sensorSet.add(r.sensor_id);
    let row = byTs.get(r.ts);
    if (!row) {
      row = { ts: r.ts };
      byTs.set(r.ts, row);
    }
    row[r.sensor_id] = r.moisture;
    row[`${r.sensor_id}_raw`] = r.raw;
  }

  const sensorIds = Array.from(sensorSet).sort();
  const rows = Array.from(byTs.values()).sort(
    (a, b) => (a.ts as number) - (b.ts as number),
  );
  return { rows, sensorIds };
}

// ── Custom tooltip ──────────────────────────────────────────────

function MoistureTooltipContent({ active, payload, label }: any) {
  if (!active || !payload?.length) return null;
  return (
    <div className="border-border/50 bg-background min-w-[10rem] rounded-lg border px-3 py-2 text-xs shadow-xl">
      <div className="mb-1.5 font-medium">
        {formatTimestamp(label as number)}
      </div>
      <div className="grid gap-1">
        {payload.map((entry: any) => {
          if (entry.type === "none") return null;
          const sensorId = entry.dataKey as string;
          const moisture = entry.value as number;
          const raw = entry.payload[`${sensorId}_raw`];
          return (
            <div key={sensorId} className="flex items-center gap-2">
              <div
                className="h-2.5 w-2.5 shrink-0 rounded-[2px]"
                style={{ backgroundColor: entry.color }}
              />
              <span className="text-muted-foreground flex-1">{sensorId}</span>
              <span className="font-mono font-medium tabular-nums">
                {(moisture * 100).toFixed(1)}%
              </span>
              {raw != null && (
                <span className="text-muted-foreground font-mono tabular-nums">
                  ({raw})
                </span>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

// ── Main component ──────────────────────────────────────────────

interface MoistureChartProps {
  zoneId?: string;
}

export function MoistureChart({ zoneId: zoneIdProp }: MoistureChartProps = {}) {
  const [timeRange, setTimeRange] = useState("6h");
  const [internalZoneId, setInternalZoneId] = useState("__all__");

  const zoneId = zoneIdProp ?? internalZoneId;
  const manageZone = zoneIdProp === undefined;

  // Derive limit from time range
  const limit = TIME_RANGES.find((r) => r.value === timeRange)?.limit ?? 360;

  // Fetch data — map "__all__" sentinel to undefined so the API returns all zones
  const { data: readings, loading } = useReadings({
    zone_id: zoneId === "__all__" ? undefined : zoneId || undefined,
    limit,
  });
  const { data: zones } = useZones();

  // Find zone config for reference lines
  const activeZone = useMemo(
    () => zones?.find((z) => z.zone_id === zoneId) ?? null,
    [zones, zoneId],
  );

  // Pivot readings into chart rows
  const { rows, sensorIds } = useMemo(
    () => pivotReadings(readings ?? []),
    [readings],
  );

  // Build chart config dynamically
  const chartConfig = useMemo(() => {
    const cfg: ChartConfig = {};
    sensorIds.forEach((id, i) => {
      cfg[id] = {
        label: id,
        color: SENSOR_COLORS[i % SENSOR_COLORS.length],
      };
    });
    return cfg;
  }, [sensorIds]);

  // Description text
  const rangeLabel =
    TIME_RANGES.find((r) => r.value === timeRange)?.label ?? "";
  const zoneLabel = activeZone?.name ?? "All Zones";

  return (
    <Card className="@container/card">
      <CardHeader>
        <CardTitle>Soil Moisture</CardTitle>
        <CardDescription>
          <span className="hidden @[540px]/card:block">
            {zoneLabel} &mdash; {rangeLabel}
          </span>
          <span className="@[540px]/card:hidden">{rangeLabel}</span>
        </CardDescription>
        <CardAction>
          <div className="flex items-center gap-2">
            {manageZone && (
              <ZoneSelector value={zoneId} onChange={setInternalZoneId} />
            )}
            {/* Desktop toggle group */}
            <ToggleGroup
              type="single"
              value={timeRange}
              onValueChange={setTimeRange}
              variant="outline"
              className="hidden *:data-[slot=toggle-group-item]:!px-4 @[767px]/card:flex"
            >
              {TIME_RANGES.map((r) => (
                <ToggleGroupItem key={r.value} value={r.value}>
                  {r.label}
                </ToggleGroupItem>
              ))}
            </ToggleGroup>
            {/* Mobile select */}
            <Select value={timeRange} onValueChange={setTimeRange}>
              <SelectTrigger
                className="flex w-32 **:data-[slot=select-value]:block **:data-[slot=select-value]:truncate @[767px]/card:hidden"
                size="sm"
                aria-label="Select time range"
              >
                <SelectValue placeholder="Last 6h" />
              </SelectTrigger>
              <SelectContent className="rounded-xl">
                {TIME_RANGES.map((r) => (
                  <SelectItem
                    key={r.value}
                    value={r.value}
                    className="rounded-lg"
                  >
                    {r.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        </CardAction>
      </CardHeader>
      <CardContent className="px-2 pt-4 sm:px-6 sm:pt-6">
        {loading && !rows.length ? (
          <div className="flex h-[250px] items-center justify-center text-sm text-muted-foreground">
            Loading moisture data…
          </div>
        ) : !rows.length ? (
          <div className="flex h-[250px] items-center justify-center text-sm text-muted-foreground">
            No readings available
          </div>
        ) : (
          <ChartContainer
            config={chartConfig}
            className="aspect-auto h-[250px] w-full"
          >
            <AreaChart data={rows}>
              <defs>
                {sensorIds.map((id, i) => (
                  <linearGradient
                    key={id}
                    id={`fill-${id}`}
                    x1="0"
                    y1="0"
                    x2="0"
                    y2="1"
                  >
                    <stop
                      offset="5%"
                      stopColor={SENSOR_COLORS[i % SENSOR_COLORS.length]}
                      stopOpacity={0.6}
                    />
                    <stop
                      offset="95%"
                      stopColor={SENSOR_COLORS[i % SENSOR_COLORS.length]}
                      stopOpacity={0.05}
                    />
                  </linearGradient>
                ))}
              </defs>
              <CartesianGrid vertical={false} />
              <XAxis
                dataKey="ts"
                tickLine={false}
                axisLine={false}
                tickMargin={8}
                minTickGap={48}
                tickFormatter={formatTime}
              />
              <YAxis
                domain={[0, 1]}
                tickLine={false}
                axisLine={false}
                tickMargin={4}
                width={42}
                tickFormatter={(v: number) => `${Math.round(v * 100)}%`}
              />
              {/* Reference lines for zone thresholds */}
              {activeZone && (
                <>
                  <ReferenceLine
                    y={activeZone.min_moisture}
                    stroke="var(--destructive)"
                    strokeDasharray="6 3"
                    label={{
                      value: `Min ${Math.round(activeZone.min_moisture * 100)}%`,
                      position: "insideTopRight",
                      fill: "var(--destructive)",
                      fontSize: 11,
                    }}
                  />
                  <ReferenceLine
                    y={activeZone.target_moisture}
                    stroke="var(--chart-2)"
                    strokeDasharray="6 3"
                    label={{
                      value: `Target ${Math.round(activeZone.target_moisture * 100)}%`,
                      position: "insideTopRight",
                      fill: "var(--chart-2)",
                      fontSize: 11,
                    }}
                  />
                </>
              )}
              <ChartTooltip
                cursor={false}
                content={<MoistureTooltipContent />}
              />
              {sensorIds.map((id, i) => (
                <Area
                  key={id}
                  dataKey={id}
                  type="monotone"
                  fill={`url(#fill-${id})`}
                  stroke={SENSOR_COLORS[i % SENSOR_COLORS.length]}
                  strokeWidth={2}
                  dot={false}
                  connectNulls
                />
              ))}
            </AreaChart>
          </ChartContainer>
        )}
      </CardContent>
    </Card>
  );
}
