import { waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { DashboardEventStream } from "./events";
import type { EventSourceLike } from "./events";

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

  emit(type: string, data: string) {
    const event = { data } as MessageEvent<string>;
    for (const listener of this.listeners.get(type) ?? []) {
      listener(event);
    }
  }
}

describe("DashboardEventStream", () => {
  it("opens the typed same-origin stream and recovers from invalid data", async () => {
    const sources: FakeEventSource[] = [];
    const urls: string[] = [];
    const resync = vi.fn(() => Promise.resolve("cursor generation/7"));
    const onModelCall = vi.fn();
    const stream = new DashboardEventStream({
      resync,
      onModelCall,
      onSourceHealth: vi.fn(),
      onAlert: vi.fn(),
      onSettings: vi.fn(),
      onStatus: vi.fn(),
      baseDelayMs: 0,
      maxDelayMs: 0,
      createEventSource(url) {
        urls.push(url);
        const source = new FakeEventSource();
        sources.push(source);
        return source;
      },
    });

    stream.start();
    await waitFor(() => {
      expect(sources).toHaveLength(1);
    });
    expect(urls[0]).toBe(
      "/api/v1/events?after_event_id=cursor%20generation%2F7",
    );
    sources[0]?.emit(
      "model-call",
      JSON.stringify({
        event_id: "event-1",
        emitted_at: "2026-07-15T04:00:00Z",
        model_call: { id: "call-1" },
      }),
    );
    expect(onModelCall).toHaveBeenCalledWith({
      event_id: "event-1",
      emitted_at: "2026-07-15T04:00:00Z",
      model_call: { id: "call-1" },
    });

    sources[0]?.emit("settings", "not-json");
    await waitFor(() => {
      expect(sources).toHaveLength(2);
    });
    expect(sources[0]?.closed).toBe(true);
    expect(resync).toHaveBeenCalledTimes(2);
    stream.stop();
  });
});
