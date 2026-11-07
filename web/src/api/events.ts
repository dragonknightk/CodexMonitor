import type {
  AlertEvent,
  ModelCallEvent,
  SettingsEvent,
  SourceHealthEvent,
  StreamControlEvent,
} from "./types";

export type StreamStatus =
  "connecting" | "live" | "recovering" | "disconnected";

export interface EventSourceLike {
  onopen: ((event: Event) => void) | null;
  onerror: ((event: Event) => void) | null;
  addEventListener(
    type: string,
    listener: (event: MessageEvent<string>) => void,
  ): void;
  close(): void;
}

export interface EventStreamCallbacks {
  resync: () => Promise<string>;
  onModelCall: (event: ModelCallEvent) => void;
  onSourceHealth: (event: SourceHealthEvent) => void;
  onAlert: (event: AlertEvent) => void;
  onSettings: (event: SettingsEvent) => void;
  onStatus: (status: StreamStatus) => void;
}

export interface EventStreamOptions extends EventStreamCallbacks {
  createEventSource?: (url: string) => EventSourceLike;
  baseDelayMs?: number;
  maxDelayMs?: number;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function parseEvent<T extends { emitted_at: string; event_id: string }>(
  data: string,
  payloadKey: keyof T,
): T | null {
  try {
    const value: unknown = JSON.parse(data) as unknown;
    if (
      !isRecord(value) ||
      typeof value.event_id !== "string" ||
      typeof value.emitted_at !== "string" ||
      !(payloadKey in value) ||
      !isRecord(value[payloadKey as string])
    ) {
      return null;
    }
    return value as T;
  } catch {
    return null;
  }
}

function parseControlEvent(data: string): StreamControlEvent | null {
  try {
    const value: unknown = JSON.parse(data) as unknown;
    if (
      !isRecord(value) ||
      typeof value.event_id !== "string" ||
      typeof value.emitted_at !== "string" ||
      (value.reason !== "overflow" &&
        value.reason !== "missed_event" &&
        value.reason !== "server_restart")
    ) {
      return null;
    }
    return value as StreamControlEvent;
  } catch {
    return null;
  }
}

function browserEventSource(url: string): EventSourceLike {
  return new EventSource(url, { withCredentials: true });
}

export class DashboardEventStream {
  private readonly callbacks: EventStreamCallbacks;
  private readonly createEventSource: (url: string) => EventSourceLike;
  private readonly baseDelayMs: number;
  private readonly maxDelayMs: number;
  private source: EventSourceLike | null = null;
  private timer: ReturnType<typeof setTimeout> | null = null;
  private stopped = true;
  private recovering = false;
  private reconnectAttempt = 0;
  private generation = 0;

  constructor(options: EventStreamOptions) {
    this.callbacks = options;
    this.createEventSource = options.createEventSource ?? browserEventSource;
    this.baseDelayMs = options.baseDelayMs ?? 500;
    this.maxDelayMs = options.maxDelayMs ?? 10_000;
  }

  start() {
    if (!this.stopped) {
      return;
    }
    this.stopped = false;
    this.callbacks.onStatus("connecting");
    this.scheduleRecovery(0);
  }

  stop() {
    this.stopped = true;
    this.recovering = false;
    this.generation += 1;
    if (this.timer !== null) {
      clearTimeout(this.timer);
      this.timer = null;
    }
    this.closeSource();
    this.callbacks.onStatus("disconnected");
  }

  private closeSource() {
    if (this.source !== null) {
      this.source.close();
      this.source = null;
    }
  }

  private recoveryDelay() {
    const exponent = Math.min(this.reconnectAttempt, 8);
    return Math.min(this.baseDelayMs * 2 ** exponent, this.maxDelayMs);
  }

  private scheduleRecovery(delay = this.recoveryDelay()) {
    if (this.stopped || this.recovering) {
      return;
    }

    this.recovering = true;
    this.generation += 1;
    const generation = this.generation;
    this.closeSource();
    this.callbacks.onStatus("recovering");
    this.timer = setTimeout(() => {
      this.timer = null;
      void this.resynchronize(generation);
    }, delay);
  }

  private async resynchronize(generation: number) {
    try {
      const cursor = await this.callbacks.resync();
      if (this.stopped || generation !== this.generation) {
        return;
      }
      this.recovering = false;
      this.connect(cursor, generation);
    } catch {
      if (this.stopped || generation !== this.generation) {
        return;
      }
      this.recovering = false;
      this.reconnectAttempt += 1;
      this.scheduleRecovery();
    }
  }

  private connect(cursor: string, generation: number) {
    if (this.stopped || generation !== this.generation) {
      return;
    }

    const url = `/api/v1/events?after_event_id=${encodeURIComponent(cursor)}`;
    const source = this.createEventSource(url);
    this.source = source;
    this.callbacks.onStatus("connecting");

    source.onopen = () => {
      if (this.source !== source || this.stopped) {
        return;
      }
      this.reconnectAttempt = 0;
      this.callbacks.onStatus("live");
    };
    source.onerror = () => {
      if (this.source === source) {
        this.scheduleRecovery();
      }
    };

    source.addEventListener("model-call", (event) => {
      const parsed = parseEvent<ModelCallEvent>(event.data, "model_call");
      if (parsed === null) {
        this.scheduleRecovery(0);
      } else if (this.source === source) {
        this.callbacks.onModelCall(parsed);
      }
    });
    source.addEventListener("source-health", (event) => {
      const parsed = parseEvent<SourceHealthEvent>(event.data, "source");
      if (parsed === null) {
        this.scheduleRecovery(0);
      } else if (this.source === source) {
        this.callbacks.onSourceHealth(parsed);
      }
    });
    source.addEventListener("alert", (event) => {
      const parsed = parseEvent<AlertEvent>(event.data, "alert");
      if (parsed === null) {
        this.scheduleRecovery(0);
      } else if (this.source === source) {
        this.callbacks.onAlert(parsed);
      }
    });
    source.addEventListener("settings", (event) => {
      const parsed = parseEvent<SettingsEvent>(event.data, "document");
      if (parsed === null) {
        this.scheduleRecovery(0);
      } else if (this.source === source) {
        this.callbacks.onSettings(parsed);
      }
    });
    source.addEventListener("stream-control", (event) => {
      if (parseControlEvent(event.data) === null) {
        this.scheduleRecovery(0);
        return;
      }
      if (this.source === source) {
        this.scheduleRecovery(0);
      }
    });
  }
}
