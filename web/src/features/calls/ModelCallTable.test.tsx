import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type { ModelCall } from "../../api/types";
import { ModelCallTable } from "./ModelCallTable";

const completeCall: ModelCall = {
  id: "019b7ba7-2d7e-7a18-bf2d-a5c3691a90d7",
  event_at: "2026-07-15T04:05:06Z",
  conversation: {
    id: "019b7ba7-2d7e-7a18-bf2d-a5c3691a90d8",
    source_id: "conversation-287",
    thread_id: "thread-934",
    resolved_display_name: "Release diagnostics",
  },
  turn_id: "019b7ba7-2d7e-7a18-bf2d-a5c3691a90d9",
  model: "gpt-5.6-terra",
  reasoning_effort: "high",
  tokens: {
    input: 12_345,
    cached_input: 2_048,
    output: 987,
    reasoning: 321,
    total: 13_332,
  },
  source_ids: ["otlp-logs", "codex-sessions"],
  client: "codex-desktop",
  surface: "desktop",
  temporal_class: "realtime",
  alert_state: "matched",
  alert_count: 2,
  field_sources: [],
};

const unknownCall: ModelCall = {
  ...completeCall,
  id: "019b7ba7-2d7e-7a18-bf2d-a5c3691a90da",
  conversation: null,
  turn_id: null,
  model: null,
  reasoning_effort: null,
  tokens: {
    input: null,
    cached_input: null,
    output: null,
    reasoning: null,
    total: null,
  },
  source_ids: [],
  client: null,
  surface: null,
  alert_state: "none",
  alert_count: 0,
};

describe("ModelCallTable", () => {
  it("renders the stable twelve-column model-call projection", () => {
    render(
      <ModelCallTable
        state={{ kind: "ready", items: [completeCall] }}
        locale="en-US"
        onRetry={vi.fn()}
      />,
    );

    const table = screen.getByRole("table", { name: "Recent model calls" });
    const columnHeaders = within(table).getAllByRole("columnheader");
    expect(columnHeaders).toHaveLength(12);
    expect(columnHeaders.map((header) => header.textContent)).toEqual([
      "Time",
      "Conversation / thread",
      "Model",
      "Effort",
      "Input",
      "Cached",
      "Output",
      "Reasoning",
      "Total",
      "Source",
      "Client",
      "Alert",
    ]);
    expect(within(table).getByText("Release diagnostics")).toBeInTheDocument();
    expect(within(table).getByText("gpt-5.6-terra")).toBeInTheDocument();
    expect(within(table).getByText("12,345")).toBeInTheDocument();
    expect(within(table).getByText("otlp-logs +1")).toHaveAttribute(
      "title",
      "otlp-logs, codex-sessions",
    );
    expect(within(table).getByText("Matched (2)")).toBeInTheDocument();
    expect(within(table).getByRole("time")).toHaveAttribute(
      "datetime",
      completeCall.event_at,
    );
  });

  it("renders explicit unknown values instead of fabricating data", () => {
    render(
      <ModelCallTable
        state={{ kind: "ready", items: [unknownCall] }}
        locale="en-US"
        onRetry={vi.fn()}
      />,
    );

    expect(screen.getAllByText("Unknown")).toHaveLength(10);
    expect(screen.getByText("None")).toBeInTheDocument();
  });

  it("keeps the table structure during loading and empty states", () => {
    const { rerender } = render(
      <ModelCallTable
        state={{ kind: "loading" }}
        locale="en-US"
        onRetry={vi.fn()}
      />,
    );

    const table = screen.getByRole("table", { name: "Recent model calls" });
    expect(table).toHaveAttribute("aria-busy", "true");
    expect(within(table).getAllByRole("columnheader")).toHaveLength(12);
    expect(screen.getByText("Loading model calls")).toBeInTheDocument();

    rerender(
      <ModelCallTable
        state={{ kind: "ready", items: [] }}
        locale="en-US"
        onRetry={vi.fn()}
      />,
    );

    expect(table).toHaveAttribute("aria-busy", "false");
    expect(screen.getByText("No model calls yet")).toBeInTheDocument();
    expect(
      screen.getByText(/completed calls will appear here/i),
    ).toBeInTheDocument();
  });

  it("localizes API failures and exposes a retry action", async () => {
    const onRetry = vi.fn();
    const user = userEvent.setup();
    render(
      <ModelCallTable
        state={{
          kind: "error",
          error: { code: "storage_unavailable", params: {} },
        }}
        locale="en-US"
        onRetry={onRetry}
      />,
    );

    expect(screen.getByRole("alert")).toHaveTextContent(
      "Local monitoring storage is temporarily unavailable.",
    );
    await user.click(screen.getByRole("button", { name: "Try again" }));
    expect(onRetry).toHaveBeenCalledOnce();
  });
});
