import { useMemo } from "preact/hooks"
import { useStatus } from "@/hooks/use-api"
import { Badge } from "@/components/ui/badge"
import { Skeleton } from "@/components/ui/skeleton"
import type { EventKind } from "@/types"

const BORDER_COLOR: Record<EventKind, string> = {
  reading: "border-l-blue-500",
  valve: "border-l-green-500",
  error: "border-l-red-500",
  system: "border-l-gray-500",
}

const BADGE_CLASS: Record<EventKind, string> = {
  reading: "bg-blue-100 text-blue-800 dark:bg-blue-900 dark:text-blue-200",
  valve: "bg-green-100 text-green-800 dark:bg-green-900 dark:text-green-200",
  error: "bg-red-100 text-red-800 dark:bg-red-900 dark:text-red-200",
  system: "bg-gray-100 text-gray-800 dark:bg-gray-900 dark:text-gray-200",
}

const MAX_EVENTS = 50

function formatIsoTs(iso: string): string {
  return new Date(iso).toLocaleString()
}

export function EventLog() {
  const { data: status, loading } = useStatus()

  const events = useMemo(
    () => (status?.events ?? []).slice(0, MAX_EVENTS),
    [status?.events],
  )

  if (loading && !status) {
    return (
      <div className="space-y-2 pt-4">
        {Array.from({ length: 8 }).map((_, i) => (
          <Skeleton key={i} className="h-12 w-full" />
        ))}
      </div>
    )
  }

  if (events.length === 0) {
    return (
      <div className="flex items-center justify-center py-12 text-sm text-muted-foreground">
        No system events
      </div>
    )
  }

  return (
    <div className="space-y-1 pt-4">
      {events.map((evt, i) => (
        <div
          key={`${evt.ts}-${i}`}
          className={`flex items-start gap-3 rounded-md border-l-4 bg-muted/30 px-3 py-2 ${BORDER_COLOR[evt.kind]}`}
        >
          <span className="shrink-0 text-xs text-muted-foreground tabular-nums pt-0.5">
            {formatIsoTs(evt.ts)}
          </span>
          <Badge
            variant="secondary"
            className={`shrink-0 text-[10px] uppercase tracking-wide ${BADGE_CLASS[evt.kind]}`}
          >
            {evt.kind}
          </Badge>
          <span className="text-sm">{evt.detail}</span>
        </div>
      ))}
    </div>
  )
}
