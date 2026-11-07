import { act, render, screen, waitFor } from "@testing-library/react";
import { StrictMode, useEffect } from "react";
import { describe, expect, it, vi } from "vitest";

import { DashboardEventStream } from "../../api/events";
import type { EventSourceLike } from "../../api/events";
import type { HealthSnapshot, ModelCallEvent } from "../../api/types";
import type { DashboardEventsApi } from "./api";
import { useDashboardSynchronization } from "./useDashboardSynchronization";

const modelCallEvent: ModelCallEvent = {
  event_id: "event-model-call-1",
  emitted_at: "2026-07-15T04:01:01Z",
  model_call: {
    id: "019b7ba7-2d7e-7a18-bf2d-a5c3691a9101",
    event_at: "2026-07-15T04:01:00Z",
    conversation: null,
    turn_id: null,
    model: "gpt-live",
    reasoning_effort: null,
    tokens: {
      input: 10,
      cached_input: null,
      output: 5,
      reasoning: null,
      total: 15,
    },
    source_ids: ["otlp-logs"],
    client: null,
    surface: null,
    temporal_class: "realtime",
    alert_state: "none",
    alert_count: 0,
    field_sources: [],
  },
};

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
    checked_at: "2026-07-15T04:00:00Z",
  };
}

function Harness({ api }: { api: DashboardEventsApi }) {
  const { epoch, modelCall, phase, reportSnapshot } =
    useDashboardSynchronization({ api });

  useEffect(() => {
    if (epoch === null) {
      return;
    }
    for (const resource of [
      "calls",
      "sources",
      "alerts",
      "settings",
    ] as const) {
      reportSnapshot(resource, epoch);
    }
  }, [epoch, reportSnapshot]);

  return (
    <output aria-label="synchronization state">
      {`${phase}:${String(modelCall.at(-1)?.deliveryId ?? 0)}`}
    </output>
  );
}

describe("useDashboardSynchronization", () => {
  it("does not restart a stream controller cleaned up by StrictMode", async () => {
    const startedControllers: number[] = [];
    const stoppedControllers: number[] = [];
    let controllerId = 0;
    const createStream = vi.fn<DashboardEventsApi["createStream"]>(() => {
      const id = controllerId;
      controllerId += 1;
      return {
        start() {
          startedControllers.push(id);
        },
        stop() {
          stoppedControllers.push(id);
        },
      };
    });

    render(
      <StrictMode>
        <Harness
          api={{
            createStream,
            getHealth: () => Promise.resolve(health("strict-mode-cursor")),
          }}
        />
      </StrictMode>,
    );

    await waitFor(() => {
      expect(createStream).toHaveBeenCalledTimes(2);
      expect(startedControllers).toEqual([1]);
    });
    expect(stoppedControllers).toContain(0);
  });

  it("deduplicates replay, resynchronizes on overflow and reconnects after disconnect", async () => {
    const sources: FakeEventSource[] = [];
    const urls: string[] = [];
    let healthIndex = 0;
    const getHealth = vi.fn<DashboardEventsApi["getHealth"]>(() => {
      healthIndex += 1;
      return Promise.resolve(health(`cursor-${String(healthIndex)}`));
    });
    const createStream = vi.fn<DashboardEventsApi["createStream"]>(
      (callbacks) =>
        new DashboardEventStream({
          ...callbacks,
          baseDelayMs: 0,
          maxDelayMs: 0,
          createEventSource(url) {
            urls.push(url);
            const source = new FakeEventSource();
            sources.push(source);
            return source;
          },
        }),
    );
    render(<Harness api={{ getHealth, createStream }} />);

    await waitFor(() => {
      expect(sources).toHaveLength(1);
    });
    expect(urls[0]).toContain("after_event_id=cursor-1");
    act(() => {
      sources[0]?.onopen?.(new Event("open"));
      sources[0]?.emit("model-call", modelCallEvent);
      sources[0]?.emit("model-call", modelCallEvent);
    });
    expect(screen.getByLabelText("synchronization state")).toHaveTextContent(
      "live:1",
    );

    act(() => {
      sources[0]?.emit("stream-control", {
        event_id: "event-control-1",
        emitted_at: "2026-07-15T04:02:00Z",
        reason: "overflow",
      });
    });
    await waitFor(() => {
      expect(sources).toHaveLength(2);
    });
    expect(urls[1]).toContain("after_event_id=cursor-2");
    expect(sources[0]?.closed).toBe(true);

    act(() => {
      sources[1]?.onerror?.(new Event("error"));
    });
    await waitFor(() => {
      expect(sources).toHaveLength(3);
    });
    expect(urls[2]).toContain("after_event_id=cursor-3");
    expect(getHealth).toHaveBeenCalledTimes(3);
  });
});
