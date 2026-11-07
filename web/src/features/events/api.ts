import { getHealth } from "../../api/client";
import { DashboardEventStream } from "../../api/events";
import type { EventStreamCallbacks } from "../../api/events";
import type { HealthSnapshot } from "../../api/types";

export interface DashboardEventStreamController {
  start: () => void;
  stop: () => void;
}

export interface DashboardEventsApi {
  createStream: (
    callbacks: EventStreamCallbacks,
  ) => DashboardEventStreamController;
  getHealth: () => Promise<HealthSnapshot>;
}

export const dashboardEventsApi: DashboardEventsApi = {
  getHealth,
  createStream(callbacks) {
    return new DashboardEventStream(callbacks);
  },
};
