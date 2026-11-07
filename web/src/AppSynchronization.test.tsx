import { act, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  Alert,
  HealthSnapshot,
  ModelCall,
  ModelCallSummary,
  SettingsDocument,
  SourceHealth,
} from "./api/types";
import { DashboardEventStream } from "./api/events";
import type { EventSourceLike } from "./api/events";
import { App } from "./App";
import type { AlertsApi } from "./features/alerts";
import type { CallsApi } from "./features/calls";
import type { DashboardEventsApi } from "./features/events";
import type { SettingsApi } from "./features/settings";
import type { SourcesApi } from "./features/sources";
import i18n from "./i18n";

const timestamp = "2026-07-15T04:01:00Z";

class FakeEventSource implements EventSourceLike {
  onopen: ((event: Event) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  closed = false;
  private readonly listeners = new Map<
    string,
    ((event: MessageEvent<string>) => void)[]
  >();

  addEventListener(
    type: string,
    listener: (event: MessageEvent<string>) => void,
  ) {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener]);
  }

  close() {
    this.closed = true;
  }

  emit(type: string, value: unknown) {
    const event = { data: JSON.stringify(value) } as MessageEvent<string>;
    for (const listener of this.listeners.get(type) ?? []) {
      listener(event);
    }
  }
}

function modelCall(id: string, model: string, second: number): ModelCall {
  return {
    id,
    event_at: `2026-07-15T04:01:${String(second).padStart(2, "0")}Z`,
    conversation: null,
    turn_id: null,
    model,
    reasoning_effort: "high",
    tokens: {
      input: 10,
      cached_input: 2,
      output: 5,
      reasoning: 3,
      total: 15,
    },
    source_ids: ["otlp-logs"],
    client: "codex-desktop",
    surface: "desktop",
    temporal_class: "realtime",
    alert_state: "none",
    alert_count: 0,
    field_sources: [],
  };
}

const callOne = modelCall(
  "019b7ba7-2d7e-7a18-bf2d-a5c3691a9101",
  "gpt-live-one",
  1,
);
const callTwo = modelCall(
  "019b7ba7-2d7e-7a18-bf2d-a5c3691a9102",
  "gpt-live-two",
  2,
);

const source: SourceHealth = {
  source_id: "otlp-logs",
  source_contract_version: 1,
  status: "healthy",
  receiver: {
    host: "127.0.0.1",
    port: 4318,
    bind_state: "bound",
    retry_supported: true,
  },
  last_accepted_at: timestamp,
  last_error_code: null,
  last_error_occurred_at: null,
  updated_at: timestamp,
  supported_fields: ["model_call.reasoning_output_tokens"],
};

const secondSource: SourceHealth = {
  ...source,
  source_id: "codex-sessions",
  source_contract_version: 2,
  receiver: null,
};

const alert: Alert = {
  id: "019b7ba7-2d7e-7a18-bf2d-a5c3691a9201",
  model_call_id: callOne.id,
  conversation_id: null,
  rule_id: "suspected-reasoning-limit-pattern",
  rule_version: 3,
  evidence: {
    reasoning_output_tokens: 516,
    pattern_n: 1,
    event_at: timestamp,
    source_id: "otlp-logs",
    source_contract_version: 1,
    observed_at: timestamp,
    quality: "direct",
  },
  action: null,
  created_at: timestamp,
};

const initialSettings: SettingsDocument = {
  revision: "7",
  settings: {
    login_autostart: false,
    system_notifications: true,
    notify_max_n: 1,
    retention_days: 90,
    language: "en-US",
  },
  applied_at: timestamp,
};

function health(cursor: string): HealthSnapshot {
  return {
    api_version: "v1",
    status: "ready",
    dashboard_listener: {
      host: "127.0.0.1",
      port: 4319,
      state: "bound",
    },
    stream_cursor: cursor,
    checked_at: timestamp,
  };
}

function summary(count: number): ModelCallSummary {
  const aggregate = {
    value: 0,
    known_call_count: count,
    total_call_count: count,
    complete: true,
  };
  return {
    call_count: count,
    alert_count: 0,
    input: aggregate,
    cached_input: aggregate,
    output: aggregate,
    reasoning: aggregate,
    total: aggregate,
  };
}

beforeEach(async () => {
  await i18n.changeLanguage("en-US");
});

afterEach(async () => {
  await i18n.changeLanguage("en-US");
});

describe("App synchronization", () => {
  it("applies typed events and performs a duplicate-free REST resync after overflow", async () => {
    const order: string[] = [];
    const eventSources: FakeEventSource[] = [];
    let calls: ModelCall[] = [];
    let sources: SourceHealth[] = [];
    let alerts: Alert[] = [];
    let settings = initialSettings;
    let healthCount = 0;

    const callsApi: CallsApi = {
      list: vi.fn<CallsApi["list"]>((query) => {
        order.push("calls");
        return Promise.resolve({
          items: [...calls],
          page: {
            limit: query.limit ?? 25,
            has_more: false,
            next_cursor: null,
          },
        });
      }),
      summarize: vi.fn(() => {
        order.push("summary");
        return Promise.resolve(summary(calls.length));
      }),
    };
    const sourcesApi: SourcesApi = {
      list: vi.fn(() => {
        order.push("sources");
        return Promise.resolve({ items: [...sources] });
      }),
      retry: vi.fn(() => Promise.resolve(source)),
    };
    const alertsApi: AlertsApi = {
      list: vi.fn<AlertsApi["list"]>(() => {
        order.push("alerts");
        return Promise.resolve({
          items: [...alerts],
          page: { limit: 10, has_more: false, next_cursor: null },
        });
      }),
    };
    const settingsApi: SettingsApi = {
      get: vi.fn(() => {
        order.push("settings");
        return Promise.resolve(settings);
      }),
      replace: vi.fn<SettingsApi["replace"]>((next) =>
        Promise.resolve({ ...settings, settings: next }),
      ),
    };
    const getHealth = vi.fn<DashboardEventsApi["getHealth"]>(() => {
      order.push("health");
      healthCount += 1;
      return Promise.resolve(health(`cursor-${String(healthCount)}`));
    });
    const createStream = vi.fn<DashboardEventsApi["createStream"]>(
      (callbacks) =>
        new DashboardEventStream({
          ...callbacks,
          baseDelayMs: 0,
          maxDelayMs: 0,
          createEventSource(url) {
            order.push(`connect:${url}`);
            const eventSource = new FakeEventSource();
            eventSources.push(eventSource);
            return eventSource;
          },
        }),
    );

    render(
      <App
        callsApi={callsApi}
        sourcesApi={sourcesApi}
        alertsApi={alertsApi}
        settingsApi={settingsApi}
        eventsApi={{ getHealth, createStream }}
      />,
    );

    await waitFor(() => {
      expect(eventSources).toHaveLength(1);
    });
    expect(order[0]).toBe("health");
    const firstConnect = order.findIndex((entry) =>
      entry.includes("after_event_id=cursor-1"),
    );
    expect(firstConnect).toBeGreaterThan(order.indexOf("settings"));
    expect(firstConnect).toBeGreaterThan(order.indexOf("summary"));

    calls = [callOne];
    sources = [source, secondSource];
    alerts = [alert];
    settings = {
      ...initialSettings,
      revision: "8",
      settings: {
        ...initialSettings.settings,
        system_notifications: false,
      },
    };
    act(() => {
      eventSources[0]?.onopen?.(new Event("open"));
      eventSources[0]?.emit("model-call", {
        event_id: "event-call-1",
        emitted_at: timestamp,
        model_call: callOne,
      });
      eventSources[0]?.emit("model-call", {
        event_id: "event-call-1",
        emitted_at: timestamp,
        model_call: callOne,
      });
      eventSources[0]?.emit("source-health", {
        event_id: "event-source-1",
        emitted_at: timestamp,
        source,
      });
      eventSources[0]?.emit("source-health", {
        event_id: "event-source-2",
        emitted_at: timestamp,
        source: secondSource,
      });
      eventSources[0]?.emit("alert", {
        event_id: "event-alert-1",
        emitted_at: timestamp,
        alert,
      });
      eventSources[0]?.emit("settings", {
        event_id: "event-settings-1",
        emitted_at: timestamp,
        document: settings,
      });
    });

    expect(await screen.findByText("gpt-live-one")).toBeInTheDocument();
    expect(screen.getAllByText("gpt-live-one")).toHaveLength(1);
    expect(await screen.findAllByText("Healthy")).toHaveLength(2);
    expect(
      await screen.findByRole("heading", {
        hidden: true,
        level: 3,
        name: "codex-sessions",
      }),
    ).toBeInTheDocument();
    expect(
      await screen.findByText("516 reasoning tokens · n = 1"),
    ).toBeInTheDocument();
    expect(
      await screen.findByRole("checkbox", {
        checked: false,
        hidden: true,
        name: /System notifications/i,
      }),
    ).toBeInTheDocument();

    calls = [callTwo, callOne];
    act(() => {
      eventSources[0]?.emit("stream-control", {
        event_id: "event-control-1",
        emitted_at: timestamp,
        reason: "overflow",
      });
    });
    await waitFor(() => {
      expect(eventSources).toHaveLength(2);
    });
    expect(eventSources[0]?.closed).toBe(true);
    expect(await screen.findByText("gpt-live-two")).toBeInTheDocument();
    const table = screen.getByRole("table", { name: "Recent model calls" });
    expect(within(table).getAllByRole("row")).toHaveLength(3);
    expect(within(table).getAllByText("gpt-live-one")).toHaveLength(1);
    expect(within(table).getAllByText("gpt-live-two")).toHaveLength(1);
  });
});
