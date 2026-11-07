import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { ApiClientError } from "../../api/client";
import type { SourceHealth } from "../../api/types";
import type { SourcesApi } from "./api";
import { SourcesView } from "./SourcesView";

const conflictedSource: SourceHealth = {
  source_id: "otlp-logs",
  source_contract_version: 1,
  status: "unavailable",
  receiver: {
    host: "127.0.0.1",
    port: 4318,
    bind_state: "conflict",
    retry_supported: true,
  },
  last_accepted_at: null,
  last_error_code: "otlp_bind_failed",
  last_error_occurred_at: "2026-07-15T04:02:00Z",
  updated_at: "2026-07-15T04:02:01Z",
  supported_fields: [
    "model_call.input_tokens",
    "model_call.reasoning_output_tokens",
  ],
};

const healthySource: SourceHealth = {
  ...conflictedSource,
  status: "healthy",
  receiver: {
    host: "127.0.0.1",
    port: 4318,
    bind_state: "bound",
    retry_supported: true,
  },
  last_error_code: null,
  last_error_occurred_at: null,
  updated_at: "2026-07-15T04:03:00Z",
};

function sourcesApi(
  list: SourcesApi["list"],
  retry: SourcesApi["retry"] = () => Promise.resolve(healthySource),
): SourcesApi {
  return { list, retry };
}

describe("SourcesView", () => {
  it("shows receiver state, safe error code, supported fields and concrete remediation", async () => {
    const list = vi
      .fn<SourcesApi["list"]>()
      .mockResolvedValue({ items: [conflictedSource] });
    render(<SourcesView api={sourcesApi(list)} locale="en-US" />);

    const card = await screen.findByRole("article");
    expect(within(card).getByText("otlp-logs")).toBeInTheDocument();
    expect(within(card).getByText("127.0.0.1:4318")).toBeInTheDocument();
    expect(within(card).getByText("Port conflict")).toBeInTheDocument();
    expect(within(card).getByText("otlp_bind_failed")).toBeInTheDocument();
    expect(within(card).getByText("Unknown")).toBeInTheDocument();
    expect(
      within(card).getByText(
        /stop or reconfigure the local process using 127\.0\.0\.1:4318/i,
      ),
    ).toBeInTheDocument();
    expect(
      within(card).getByText("model_call.reasoning_output_tokens"),
    ).toBeInTheDocument();
  });

  it("retries a conflicted receiver and replaces only its returned health snapshot", async () => {
    const retryResult = Promise.withResolvers<SourceHealth>();
    const retry = vi
      .fn<SourcesApi["retry"]>()
      .mockReturnValue(retryResult.promise);
    const user = userEvent.setup();
    render(
      <SourcesView
        api={sourcesApi(
          () => Promise.resolve({ items: [conflictedSource] }),
          retry,
        )}
        locale="en-US"
      />,
    );

    await user.click(
      await screen.findByRole("button", { name: "Retry receiver" }),
    );
    expect(retry).toHaveBeenCalledWith("otlp-logs");
    expect(
      screen.getByRole("button", { name: "Requesting retry…" }),
    ).toBeDisabled();
    retryResult.resolve(healthySource);

    expect(await screen.findByText("Healthy")).toBeInTheDocument();
    expect(screen.queryByText("otlp_bind_failed")).not.toBeInTheDocument();
    expect(screen.getByText(/receiver retry requested/i)).toBeInTheDocument();
  });

  it("renders stable API errors without exposing raw failures", async () => {
    render(
      <SourcesView
        api={sourcesApi(() =>
          Promise.reject(new ApiClientError("storage_unavailable", {})),
        )}
        locale="en-US"
      />,
    );

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Local monitoring storage is temporarily unavailable.",
    );
  });
});
