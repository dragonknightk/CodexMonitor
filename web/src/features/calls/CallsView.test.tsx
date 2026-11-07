import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { ApiClientError } from "../../api/client";
import type { ModelCallPage, ModelCallSummary } from "../../api/types";
import type { CallsApi } from "./api";
import { CallsView } from "./CallsView";

const emptyPage: ModelCallPage = {
  items: [],
  page: { limit: 25, has_more: false, next_cursor: null },
};

function aggregate(value: number, known: number, total: number) {
  return {
    value,
    known_call_count: known,
    total_call_count: total,
    complete: known === total,
  };
}

const summary: ModelCallSummary = {
  call_count: 42,
  alert_count: 2,
  input: aggregate(120_000, 42, 42),
  cached_input: aggregate(40_000, 40, 42),
  output: aggregate(12_000, 42, 42),
  reasoning: aggregate(4_000, 39, 42),
  total: aggregate(132_000, 42, 42),
};

function apiWithList(
  list: CallsApi["list"],
  summarize: CallsApi["summarize"] = () => Promise.resolve(summary),
): CallsApi {
  return { list, summarize };
}

describe("CallsView", () => {
  it("requests the deterministic initial page and matching summary", async () => {
    const list = vi.fn<CallsApi["list"]>().mockResolvedValue(emptyPage);
    const summarize = vi.fn<CallsApi["summarize"]>().mockResolvedValue(summary);
    render(<CallsView api={apiWithList(list, summarize)} />);

    expect(await screen.findByText("No model calls yet")).toBeInTheDocument();
    expect(list).toHaveBeenCalledWith({
      limit: 25,
      alert: "any",
      sort: "event_at_desc",
    });
    expect(summarize).toHaveBeenCalledWith({ alert: "any" });
    const summaryRegion = screen.getByRole("region", {
      name: "Filtered model-call summary",
    });
    expect(within(summaryRegion).getByText("42")).toBeInTheDocument();
    expect(
      within(summaryRegion).getByText("40 of 42 calls known"),
    ).toBeInTheDocument();
  });

  it("maps request failures to localized errors and retries the current page", async () => {
    const retryPage = Promise.withResolvers<ModelCallPage>();
    const list = vi
      .fn<CallsApi["list"]>()
      .mockRejectedValueOnce(new ApiClientError("storage_unavailable", {}))
      .mockReturnValueOnce(retryPage.promise);
    const user = userEvent.setup();
    render(<CallsView api={apiWithList(list)} />);

    const errorMessage = await screen.findByText(
      "Local monitoring storage is temporarily unavailable.",
    );
    const tableAlert = errorMessage.closest('[role="alert"]');
    if (!(tableAlert instanceof HTMLElement)) {
      throw new Error("model-call error is missing its alert container");
    }
    await user.click(
      within(tableAlert).getByRole("button", { name: "Try again" }),
    );
    expect(screen.getByText("Loading model calls")).toBeInTheDocument();
    retryPage.resolve(emptyPage);
    expect(await screen.findByText("No model calls yet")).toBeInTheDocument();
    expect(list).toHaveBeenCalledTimes(2);
  });

  it("normalizes every supported filter and applies it to rows and summary", async () => {
    const list = vi.fn<CallsApi["list"]>().mockResolvedValue(emptyPage);
    const summarize = vi.fn<CallsApi["summarize"]>().mockResolvedValue(summary);
    const user = userEvent.setup();
    render(<CallsView api={apiWithList(list, summarize)} />);
    await screen.findByText("No model calls yet");

    await user.selectOptions(screen.getByLabelText("Recent count"), "50");
    await user.type(
      screen.getByLabelText("Conversation / thread"),
      "  release diagnostics  ",
    );
    await user.type(screen.getByLabelText("Model"), "  gpt-5.6-terra  ");
    await user.type(screen.getByLabelText("Source"), "  otlp-logs  ");
    fireEvent.change(screen.getByLabelText("From"), {
      target: { value: "2026-07-14T08:30" },
    });
    fireEvent.change(screen.getByLabelText("To (exclusive)"), {
      target: { value: "2026-07-15T08:30" },
    });
    await user.selectOptions(screen.getByLabelText("Alert status"), "matched");
    await user.selectOptions(
      screen.getByLabelText("Sort"),
      "total_tokens_desc",
    );
    await user.click(screen.getByRole("button", { name: "Apply filters" }));

    const from = new Date("2026-07-14T08:30").toISOString();
    const to = new Date("2026-07-15T08:30").toISOString();
    await waitFor(() => {
      expect(list).toHaveBeenCalledTimes(2);
    });
    expect(list).toHaveBeenLastCalledWith({
      limit: 50,
      conversation: "release diagnostics",
      model: "gpt-5.6-terra",
      source_id: "otlp-logs",
      from,
      to,
      alert: "matched",
      sort: "total_tokens_desc",
    });
    expect(summarize).toHaveBeenLastCalledWith({
      conversation: "release diagnostics",
      model: "gpt-5.6-terra",
      source_id: "otlp-logs",
      from,
      to,
      alert: "matched",
    });

    await user.click(screen.getByRole("button", { name: "Clear filters" }));
    await waitFor(() => {
      expect(list).toHaveBeenCalledTimes(3);
    });
    expect(list).toHaveBeenLastCalledWith({
      limit: 25,
      alert: "any",
      sort: "event_at_desc",
    });
    expect(summarize).toHaveBeenLastCalledWith({ alert: "any" });
  });

  it("walks opaque cursor pages forward and backward without reusing the summary query", async () => {
    const cursor = "opaque_cursor_for_second_page_0001";
    const firstPage: ModelCallPage = {
      items: [],
      page: { limit: 25, has_more: true, next_cursor: cursor },
    };
    const list = vi
      .fn<CallsApi["list"]>()
      .mockResolvedValueOnce(firstPage)
      .mockResolvedValueOnce(emptyPage)
      .mockResolvedValueOnce(firstPage);
    const summarize = vi.fn<CallsApi["summarize"]>().mockResolvedValue(summary);
    const user = userEvent.setup();
    render(<CallsView api={apiWithList(list, summarize)} />);

    const next = await screen.findByRole("button", { name: "Next" });
    expect(next).toBeEnabled();
    await user.click(next);
    await waitFor(() => {
      expect(list).toHaveBeenNthCalledWith(2, {
        limit: 25,
        alert: "any",
        sort: "event_at_desc",
        cursor,
      });
    });
    expect(screen.getByText("Page 2")).toBeInTheDocument();

    const previous = screen.getByRole("button", { name: "Previous" });
    expect(previous).toBeEnabled();
    await user.click(previous);
    await waitFor(() => {
      expect(list).toHaveBeenNthCalledWith(3, {
        limit: 25,
        alert: "any",
        sort: "event_at_desc",
      });
    });
    expect(screen.getByText("Page 1")).toBeInTheDocument();
    expect(summarize).toHaveBeenCalledTimes(1);
  });

  it("rejects an inverted time range before issuing another query", async () => {
    const list = vi.fn<CallsApi["list"]>().mockResolvedValue(emptyPage);
    const user = userEvent.setup();
    render(<CallsView api={apiWithList(list)} />);
    await screen.findByText("No model calls yet");

    fireEvent.change(screen.getByLabelText("From"), {
      target: { value: "2026-07-16T08:30" },
    });
    fireEvent.change(screen.getByLabelText("To (exclusive)"), {
      target: { value: "2026-07-15T08:30" },
    });
    await user.click(screen.getByRole("button", { name: "Apply filters" }));

    expect(
      screen.getByRole("alert", {
        name: "",
      }),
    ).toHaveTextContent("The start time must be earlier than the end time.");
    expect(list).toHaveBeenCalledOnce();
  });
});
