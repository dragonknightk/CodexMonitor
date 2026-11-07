import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type { Alert, AlertPage } from "../../api/types";
import type { AlertsApi } from "./api";
import { AlertsView } from "./AlertsView";

const baseAlert: Alert = {
  id: "019b7ba7-2d7e-7a18-bf2d-a5c3691a9001",
  model_call_id: "019b7ba7-2d7e-7a18-bf2d-a5c3691a9002",
  conversation_id: "019b7ba7-2d7e-7a18-bf2d-a5c3691a9003",
  rule_id: "suspected-reasoning-limit-pattern",
  rule_version: 3,
  evidence: {
    reasoning_output_tokens: 516,
    pattern_n: 1,
    event_at: "2026-07-15T04:01:00Z",
    source_id: "otlp-logs",
    source_contract_version: 1,
    observed_at: "2026-07-15T04:01:01Z",
    quality: "direct",
  },
  action: {
    action_type: "system_notification",
    state: "suppressed",
    created_at: "2026-07-15T04:01:02Z",
    not_before: "2026-07-15T04:01:02Z",
    started_at: null,
    completed_at: "2026-07-15T04:01:02Z",
    error_code: null,
    suppression_reason: "notifications_disabled",
  },
  created_at: "2026-07-15T04:01:02Z",
};

const noActionAlert: Alert = {
  ...baseAlert,
  id: "019b7ba7-2d7e-7a18-bf2d-a5c3691a9011",
  model_call_id: "019b7ba7-2d7e-7a18-bf2d-a5c3691a9012",
  evidence: {
    ...baseAlert.evidence,
    reasoning_output_tokens: 2070,
    pattern_n: 4,
  },
  action: null,
};

const failedActionAlert: Alert = {
  ...baseAlert,
  id: "019b7ba7-2d7e-7a18-bf2d-a5c3691a9021",
  model_call_id: "019b7ba7-2d7e-7a18-bf2d-a5c3691a9022",
  action: {
    action_type: "system_notification",
    state: "failed",
    created_at: "2026-07-15T04:01:02Z",
    not_before: "2026-07-15T04:01:02Z",
    started_at: "2026-07-15T04:01:03Z",
    completed_at: "2026-07-15T04:01:04Z",
    error_code: "notification_dispatch_failed",
    suppression_reason: null,
  },
};

const alertPage: AlertPage = {
  items: [baseAlert, noActionAlert, failedActionAlert],
  page: { limit: 10, has_more: false, next_cursor: null },
};

describe("AlertsView", () => {
  it("shows matched evidence, rule identity, model call and independent action states", async () => {
    render(
      <AlertsView
        api={{ list: () => Promise.resolve(alertPage) }}
        locale="en-US"
      />,
    );

    expect(await screen.findAllByRole("article")).toHaveLength(3);
    expect(screen.getAllByText("516 reasoning tokens · n = 1")).toHaveLength(2);
    expect(
      screen.getAllByText("suspected-reasoning-limit-pattern@3"),
    ).toHaveLength(3);
    expect(screen.getAllByText("otlp-logs@1")).toHaveLength(3);
    expect(screen.getAllByText("Direct evidence")).toHaveLength(3);
    expect(
      screen.getByText("System notifications were disabled."),
    ).toBeInTheDocument();
    expect(screen.getByText("Not requested")).toBeInTheDocument();
    expect(
      screen.getByText("notification_dispatch_failed"),
    ).toBeInTheDocument();
    expect(
      screen.getByText("The host could not dispatch the system notification."),
    ).toBeInTheDocument();
  });

  it("appends the next deterministic alert page using only its opaque cursor", async () => {
    const cursor = "opaque_alert_cursor_for_page_two_01";
    const firstPage: AlertPage = {
      items: [baseAlert],
      page: { limit: 10, has_more: true, next_cursor: cursor },
    };
    const secondPage: AlertPage = {
      items: [baseAlert, noActionAlert],
      page: { limit: 10, has_more: false, next_cursor: null },
    };
    const list = vi
      .fn<AlertsApi["list"]>()
      .mockResolvedValueOnce(firstPage)
      .mockResolvedValueOnce(secondPage);
    const user = userEvent.setup();
    render(<AlertsView api={{ list }} locale="en-US" />);

    await user.click(
      await screen.findByRole("button", { name: "Load more alerts" }),
    );
    await waitFor(() => {
      expect(list).toHaveBeenNthCalledWith(2, { limit: 10, cursor });
    });
    expect(screen.getAllByRole("article")).toHaveLength(2);
    expect(
      screen.getByText("2,070 reasoning tokens · n = 4"),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Load more alerts" }),
    ).not.toBeInTheDocument();
  });
});
