import { IconWifiOff } from "@tabler/icons-react";

interface ConnectionBannerProps {
  error: Error | null;
}

/**
 * Thin banner shown at the top of the dashboard when the hub API is
 * unreachable.  Disappears automatically once connectivity is restored
 * (the polling hook clears the error on success).
 */
export function ConnectionBanner({ error }: ConnectionBannerProps) {
  if (!error) return null;

  return (
    <div
      role="alert"
      className="flex items-center gap-2 bg-destructive/15 px-4 py-2 text-sm text-destructive lg:px-6"
    >
      <IconWifiOff className="size-4 shrink-0" />
      <span>
        Unable to reach the hub &mdash; data may be stale.{" "}
        <span className="text-muted-foreground">({error.message})</span>
      </span>
    </div>
  );
}
