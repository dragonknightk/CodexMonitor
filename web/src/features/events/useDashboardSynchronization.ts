import { useCallback, useEffect, useRef, useState } from "react";

import type { StreamStatus } from "../../api/events";
import type {
  AlertEvent,
  ModelCallEvent,
  SettingsEvent,
  SourceHealthEvent,
} from "../../api/types";
import { dashboardEventsApi } from "./api";
import type { DashboardEventsApi } from "./api";

export type DashboardResource = "calls" | "sources" | "alerts" | "settings";
export type SynchronizationPhase = StreamStatus;

export interface DashboardEventDelivery<T> {
  deliveryId: number;
  event: T;
}

export interface ResourceSynchronization<T> {
  epoch: number | null;
  events: readonly DashboardEventDelivery<T>[];
  onSnapshotComplete: (resource: DashboardResource, epoch: number) => void;
}

interface EventDeliveries {
  alert: DashboardEventDelivery<AlertEvent>[];
  modelCall: DashboardEventDelivery<ModelCallEvent>[];
  settings: DashboardEventDelivery<SettingsEvent>[];
  sourceHealth: DashboardEventDelivery<SourceHealthEvent>[];
}

interface PendingSnapshot {
  cursor: string;
  generation: number;
  reject: (reason: Error) => void;
  resolve: (cursor: string) => void;
}

export interface DashboardSynchronization extends EventDeliveries {
  epoch: number | null;
  phase: SynchronizationPhase;
  reportSnapshot: (resource: DashboardResource, epoch: number) => void;
}

export interface DashboardSynchronizationOptions {
  api?: DashboardEventsApi | undefined;
}

const requiredResources = new Set<DashboardResource>([
  "calls",
  "sources",
  "alerts",
  "settings",
]);
const maxRememberedEventIds = 2048;
const maxBufferedDeliveriesPerResource = 256;

const emptyDeliveries: EventDeliveries = {
  alert: [],
  modelCall: [],
  settings: [],
  sourceHealth: [],
};

export function useDashboardSynchronization({
  api = dashboardEventsApi,
}: DashboardSynchronizationOptions = {}): DashboardSynchronization {
  const [epoch, setEpoch] = useState<number | null>(null);
  const [phase, setPhase] = useState<SynchronizationPhase>("disconnected");
  const [deliveries, setDeliveries] =
    useState<EventDeliveries>(emptyDeliveries);
  const mountedRef = useRef(false);
  const generationRef = useRef(0);
  const readyResourcesRef = useRef(new Set<DashboardResource>());
  const pendingSnapshotRef = useRef<PendingSnapshot | null>(null);
  const deliveryIdRef = useRef(0);
  const seenEventIdsRef = useRef(new Set<string>());
  const seenEventOrderRef = useRef<string[]>([]);

  const rememberEvent = useCallback((eventId: string): boolean => {
    if (seenEventIdsRef.current.has(eventId)) {
      return false;
    }
    seenEventIdsRef.current.add(eventId);
    seenEventOrderRef.current.push(eventId);
    if (seenEventOrderRef.current.length > maxRememberedEventIds) {
      const expired = seenEventOrderRef.current.shift();
      if (expired !== undefined) {
        seenEventIdsRef.current.delete(expired);
      }
    }
    return true;
  }, []);

  const appendDelivery = useCallback(
    <T>(
      current: DashboardEventDelivery<T>[],
      event: T,
    ): DashboardEventDelivery<T>[] =>
      [...current, { deliveryId: ++deliveryIdRef.current, event }].slice(
        -maxBufferedDeliveriesPerResource,
      ),
    [],
  );

  const deliverModelCall = useCallback(
    (event: ModelCallEvent) => {
      if (!rememberEvent(event.event_id)) {
        return;
      }
      setDeliveries((current) => ({
        ...current,
        modelCall: appendDelivery(current.modelCall, event),
      }));
    },
    [appendDelivery, rememberEvent],
  );

  const deliverSourceHealth = useCallback(
    (event: SourceHealthEvent) => {
      if (!rememberEvent(event.event_id)) {
        return;
      }
      setDeliveries((current) => ({
        ...current,
        sourceHealth: appendDelivery(current.sourceHealth, event),
      }));
    },
    [appendDelivery, rememberEvent],
  );

  const deliverAlert = useCallback(
    (event: AlertEvent) => {
      if (!rememberEvent(event.event_id)) {
        return;
      }
      setDeliveries((current) => ({
        ...current,
        alert: appendDelivery(current.alert, event),
      }));
    },
    [appendDelivery, rememberEvent],
  );

  const deliverSettings = useCallback(
    (event: SettingsEvent) => {
      if (!rememberEvent(event.event_id)) {
        return;
      }
      setDeliveries((current) => ({
        ...current,
        settings: appendDelivery(current.settings, event),
      }));
    },
    [appendDelivery, rememberEvent],
  );

  const resynchronize = useCallback(async (): Promise<string> => {
    const generation = ++generationRef.current;
    pendingSnapshotRef.current?.reject(
      new Error("dashboard snapshot was superseded"),
    );
    pendingSnapshotRef.current = null;
    readyResourcesRef.current.clear();
    setEpoch(null);

    const health = await api.getHealth();
    if (!mountedRef.current || generation !== generationRef.current) {
      throw new Error("dashboard synchronization stopped");
    }
    return new Promise<string>((resolve, reject) => {
      pendingSnapshotRef.current = {
        cursor: health.stream_cursor,
        generation,
        reject,
        resolve,
      };
      setEpoch(generation);
    });
  }, [api]);

  const reportSnapshot = useCallback(
    (resource: DashboardResource, completedEpoch: number) => {
      const pending = pendingSnapshotRef.current;
      if (pending?.generation !== completedEpoch) {
        return;
      }
      readyResourcesRef.current.add(resource);
      if (
        [...requiredResources].every((required) =>
          readyResourcesRef.current.has(required),
        )
      ) {
        pendingSnapshotRef.current = null;
        pending.resolve(pending.cursor);
      }
    },
    [],
  );

  useEffect(() => {
    let active = true;
    mountedRef.current = true;
    const stream = api.createStream({
      resync: resynchronize,
      onModelCall: deliverModelCall,
      onSourceHealth: deliverSourceHealth,
      onAlert: deliverAlert,
      onSettings: deliverSettings,
      onStatus(status) {
        if (mountedRef.current) {
          setPhase(status);
        }
      },
    });
    queueMicrotask(() => {
      if (active && mountedRef.current) {
        stream.start();
      }
    });
    return () => {
      active = false;
      mountedRef.current = false;
      generationRef.current += 1;
      pendingSnapshotRef.current?.reject(
        new Error("dashboard synchronization stopped"),
      );
      pendingSnapshotRef.current = null;
      stream.stop();
    };
  }, [
    api,
    deliverAlert,
    deliverModelCall,
    deliverSettings,
    deliverSourceHealth,
    resynchronize,
  ]);

  return { epoch, phase, reportSnapshot, ...deliveries };
}
