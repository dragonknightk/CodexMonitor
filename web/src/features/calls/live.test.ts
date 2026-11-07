import { describe, expect, it } from "vitest";

import type { ModelCall } from "../../api/types";
import { mergeModelCallEvent } from "./live";

function call(id: string, eventAt: string, model = "gpt-live"): ModelCall {
  return {
    id,
    event_at: eventAt,
    conversation: null,
    turn_id: null,
    model,
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
  };
}

describe("mergeModelCallEvent", () => {
  it("upserts by stable ID, preserves sort order and enforces filters", () => {
    const older = call("call-1", "2026-07-15T04:00:00Z");
    const newer = call("call-2", "2026-07-15T04:01:00Z");
    const query = {
      limit: 2,
      model: "gpt-live",
      alert: "none" as const,
      sort: "event_at_desc" as const,
    };

    const merged = mergeModelCallEvent([older], newer, query, true);
    expect(merged.map((item) => item.id)).toEqual(["call-2", "call-1"]);
    expect(
      mergeModelCallEvent(
        merged,
        { ...newer, tokens: { ...newer.tokens, input: 20 } },
        query,
        true,
      ),
    ).toEqual([{ ...newer, tokens: { ...newer.tokens, input: 20 } }, older]);

    const noLongerMatching = { ...newer, alert_state: "matched" as const };
    expect(
      mergeModelCallEvent(merged, noLongerMatching, query, true).map(
        (item) => item.id,
      ),
    ).toEqual(["call-1"]);
  });

  it("does not insert an unknown live row into an opaque later page", () => {
    const visible = call("call-2", "2026-07-15T04:01:00Z");
    const incoming = call("call-3", "2026-07-15T04:02:00Z");

    expect(
      mergeModelCallEvent(
        [visible],
        incoming,
        { limit: 25, alert: "any", sort: "event_at_desc" },
        false,
      ),
    ).toEqual([visible]);
  });
});
