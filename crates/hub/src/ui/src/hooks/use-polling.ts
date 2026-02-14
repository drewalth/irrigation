import { useCallback, useEffect, useRef, useState } from "preact/hooks";

export interface UsePollingResult<T> {
  data: T | null;
  error: Error | null;
  loading: boolean;
  refetch: () => void;
}

/**
 * Generic polling hook. Calls `fetcher` immediately, then every `intervalMs`.
 * Automatically cleans up on unmount. Skips overlapping requests.
 */
export function usePolling<T>(
  fetcher: () => Promise<T>,
  intervalMs: number,
): UsePollingResult<T> {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<Error | null>(null);
  const [loading, setLoading] = useState(true);
  const inflightRef = useRef(false);
  const fetcherRef = useRef(fetcher);
  fetcherRef.current = fetcher;

  const doFetch = useCallback(async () => {
    if (inflightRef.current) return;
    inflightRef.current = true;
    try {
      const result = await fetcherRef.current();
      setData(result);
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err : new Error(String(err)));
    } finally {
      inflightRef.current = false;
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    doFetch();
    const id = setInterval(doFetch, intervalMs);
    return () => clearInterval(id);
  }, [doFetch, intervalMs]);

  return { data, error, loading, refetch: doFetch };
}
