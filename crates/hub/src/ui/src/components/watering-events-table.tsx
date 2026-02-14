import { useMemo, useState } from "preact/hooks"
import { useWateringEvents, useZones } from "@/hooks/use-api"
import { DataTable, type Column } from "@/components/data-table"
import { ZoneSelector } from "@/components/zone-selector"
import { Badge } from "@/components/ui/badge"
import { Button } from "@/components/ui/button"
import { Skeleton } from "@/components/ui/skeleton"
import { IconChevronLeft, IconChevronRight } from "@tabler/icons-react"
import type { WateringEventRow } from "@/types"

const PAGE_SIZE = 10

function formatDuration(start: number, end: number): string {
  const diff = Math.max(0, end - start)
  const mins = Math.floor(diff / 60)
  const secs = diff % 60
  if (mins === 0) return `${secs}s`
  return `${mins}m ${secs}s`
}

function formatTs(epoch: number): string {
  return new Date(epoch * 1000).toLocaleString()
}

export function WateringEventsTable() {
  const [zoneFilter, setZoneFilter] = useState("__all__")
  const [offset, setOffset] = useState(0)

  const params = useMemo(
    () => ({
      zone_id: zoneFilter === "__all__" ? undefined : zoneFilter,
      limit: PAGE_SIZE,
      offset,
    }),
    [zoneFilter, offset],
  )

  const { data: events, loading } = useWateringEvents(params)
  const { data: zones } = useZones()

  const zoneMap = useMemo(() => {
    const m = new Map<string, string>()
    for (const z of zones ?? []) m.set(z.zone_id, z.name)
    return m
  }, [zones])

  const columns: Column<WateringEventRow>[] = useMemo(
    () => [
      {
        key: "zone",
        header: "Zone",
        cell: (row) => zoneMap.get(row.zone_id) ?? row.zone_id,
      },
      {
        key: "start",
        header: "Start Time",
        cell: (row) => formatTs(row.ts_start),
      },
      {
        key: "end",
        header: "End Time",
        cell: (row) => formatTs(row.ts_end),
      },
      {
        key: "duration",
        header: "Duration",
        cell: (row) => formatDuration(row.ts_start, row.ts_end),
      },
      {
        key: "reason",
        header: "Reason",
        cell: (row) => row.reason,
      },
      {
        key: "result",
        header: "Result",
        cell: (row) => (
          <Badge
            variant={row.result === "ok" ? "default" : "destructive"}
            className={
              row.result === "ok"
                ? "bg-green-600 hover:bg-green-600/90"
                : undefined
            }
          >
            {row.result}
          </Badge>
        ),
      },
    ],
    [zoneMap],
  )

  const handleZoneChange = (value: string) => {
    setZoneFilter(value)
    setOffset(0)
  }

  if (loading && !events) {
    return (
      <div className="space-y-3 pt-4">
        {Array.from({ length: 5 }).map((_, i) => (
          <Skeleton key={i} className="h-10 w-full" />
        ))}
      </div>
    )
  }

  return (
    <div className="space-y-4 pt-4">
      <div className="flex items-center gap-2">
        <ZoneSelector value={zoneFilter} onChange={handleZoneChange} />
      </div>

      <DataTable
        columns={columns}
        data={events ?? []}
        emptyMessage="No watering events found"
      />

      <div className="flex items-center justify-end gap-2">
        <Button
          variant="outline"
          size="sm"
          disabled={offset === 0}
          onClick={() => setOffset(Math.max(0, offset - PAGE_SIZE))}
        >
          <IconChevronLeft className="size-4" />
          Previous
        </Button>
        <Button
          variant="outline"
          size="sm"
          disabled={(events?.length ?? 0) < PAGE_SIZE}
          onClick={() => setOffset(offset + PAGE_SIZE)}
        >
          Next
          <IconChevronRight className="size-4" />
        </Button>
      </div>
    </div>
  )
}
