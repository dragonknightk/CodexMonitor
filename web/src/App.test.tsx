import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { App } from "./App";
import type { AlertsApi } from "./features/alerts";
import type { CallsApi } from "./features/calls";
import type { DashboardEventsApi } from "./features/events";
import type { SourcesApi } from "./features/sources";
import type { SettingsApi } from "./features/settings";

const pendingCallsApi: CallsApi = {
  list: () => new Promise(() => undefined),
  summarize: () => new Promise(() => undefined),
};

const pendingAlertsApi: AlertsApi = {
  list: () => new Promise(() => undefined),
};

const pendingSourcesApi: SourcesApi = {
  list: () => new Promise(() => undefined),
  retry: () => new Promise(() => undefined),
};

const pendingSettingsApi: SettingsApi = {
  get: () => new Promise(() => undefined),
  replace: () => new Promise(() => undefined),
};

const pendingEventsApi: DashboardEventsApi = {
  getHealth: () => new Promise(() => undefined),
  createStream: () => ({
    start: () => undefined,
    stop: () => undefined,
  }),
};

describe("App", () => {
  it("renders a tabbed workspace and switches between feature views", async () => {
    const user = userEvent.setup();
    render(
      <App
        callsApi={pendingCallsApi}
        alertsApi={pendingAlertsApi}
        sourcesApi={pendingSourcesApi}
        settingsApi={pendingSettingsApi}
        eventsApi={pendingEventsApi}
      />,
    );

    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent(
      "CodexMonitor",
    );
    expect(
      screen.getByRole("tab", { name: "Model calls", selected: true }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("heading", { level: 2, name: "Model calls" }),
    ).toBeVisible();

    await user.click(screen.getByRole("tab", { name: "Reasoning alerts" }));
    expect(
      screen.getByRole("heading", { level: 2, name: "Reasoning alerts" }),
    ).toBeVisible();

    await user.click(screen.getByRole("tab", { name: "Sources" }));
    expect(
      screen.getByRole("heading", { level: 2, name: "Sources" }),
    ).toBeVisible();

    await user.click(screen.getByRole("tab", { name: "Settings" }));
    expect(
      screen.getByRole("heading", { level: 2, name: "Settings" }),
    ).toBeVisible();
  });
});
