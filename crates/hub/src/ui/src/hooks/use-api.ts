import { useMemo } from "preact/hooks";
import {
  fetchCounters,
  fetchReadings,
  fetchSensors,
  fetchStatus,
  fetchWateringEvents,
  fetchZones,
} from "../api";
import type {
  DailyCounters,
  ReadingRow,
  ReadingsParams,
  SensorConfig,
  StatusResponse,
  WateringEventRow,
  WateringEventsParams,
  ZoneConfig,
} from "../types";
import { usePolling, type UsePollingResult } from "./use-polling";

const FAST = 5_000; // status / live data
const SLOW = 30_000; // readings, events, config

export function useStatus(): UsePollingResult<StatusResponse> {
  return usePolling(fetchStatus, FAST);
}

export function useZones(): UsePollingResult<ZoneConfig[]> {
  return usePolling(fetchZones, SLOW);
}

export function useSensors(): UsePollingResult<SensorConfig[]> {
  return usePolling(fetchSensors, SLOW);
}

export function useReadings(params: ReadingsParams = {}): UsePollingResult<ReadingRow[]> {
  const fetcher = useMemo(
    () => () => fetchReadings(params),
    [params.sensor_id, params.zone_id, params.limit, params.offset],
  );
  return usePolling(fetcher, SLOW);
}

export function useWateringEvents(params: WateringEventsParams = {}): UsePollingResult<WateringEventRow[]> {
  const fetcher = useMemo(
    () => () => fetchWateringEvents(params),
    [params.zone_id, params.limit, params.offset],
  );
  return usePolling(fetcher, SLOW);
}

export function useCounters(zoneId: string, day?: string): UsePollingResult<DailyCounters> {
  const fetcher = useMemo(
    () => () => fetchCounters(zoneId, day),
    [zoneId, day],
  );
  return usePolling(fetcher, SLOW);
}
