import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ApiClientError } from "../../api/client";
import type { Settings, SettingsDocument } from "../../api/types";
import i18n from "../../i18n";
import type { SettingsApi } from "./api";
import { SettingsView } from "./SettingsView";

const initialSettings: Settings = {
  language: "en-US",
  login_autostart: false,
  notify_max_n: 1,
  retention_days: 90,
  system_notifications: true,
};

const initialDocument: SettingsDocument = {
  revision: "7",
  settings: initialSettings,
  applied_at: "2026-07-15T04:00:00Z",
};

function documentWith(settings: Settings, revision = "8"): SettingsDocument {
  return {
    revision,
    settings,
    applied_at: "2026-07-15T04:05:00Z",
  };
}

function apiWith(
  get: SettingsApi["get"],
  replace: SettingsApi["replace"] = (settings) =>
    Promise.resolve(documentWith(settings)),
): SettingsApi {
  return { get, replace };
}

beforeEach(async () => {
  await i18n.changeLanguage("en-US");
});

afterEach(async () => {
  await i18n.changeLanguage("en-US");
});

describe("SettingsView", () => {
  it("loads every v1 setting with the current revision metadata", async () => {
    render(
      <SettingsView
        api={apiWith(() => Promise.resolve(initialDocument))}
        locale="en-US"
      />,
    );

    expect(
      await screen.findByRole("form", { name: "CodexMonitor settings" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("checkbox", { name: /Start at login/i }),
    ).not.toBeChecked();
    expect(
      screen.getByRole("checkbox", { name: /System notifications/i }),
    ).toBeChecked();
    expect(screen.getByRole("spinbutton", { name: "Maximum n" })).toHaveValue(
      1,
    );
    expect(
      screen.getByRole("combobox", {
        name: /Structured history retention/i,
      }),
    ).toHaveValue("90");
  });

  it("applies the persisted locale when settings first load", async () => {
    const persisted = documentWith(
      { ...initialSettings, language: "zh-CN" },
      "7",
    );
    render(
      <SettingsView
        api={apiWith(() => Promise.resolve(persisted))}
        locale="en-US"
      />,
    );

    expect(
      await screen.findByRole("heading", { name: "设置" }),
    ).toBeInTheDocument();
    expect(i18n.resolvedLanguage).toBe("zh-CN");
  });

  it("replaces the complete document with its current revision", async () => {
    const replace = vi
      .fn<SettingsApi["replace"]>()
      .mockImplementation((settings) =>
        Promise.resolve(documentWith(settings)),
      );
    const user = userEvent.setup();
    render(
      <SettingsView
        api={apiWith(() => Promise.resolve(initialDocument), replace)}
        locale="en-US"
      />,
    );

    await user.click(
      await screen.findByRole("checkbox", { name: /Start at login/i }),
    );
    await user.click(
      screen.getByRole("checkbox", { name: /System notifications/i }),
    );
    await user.selectOptions(
      screen.getByRole("combobox", { name: "Threshold mode" }),
      "all",
    );
    await user.selectOptions(
      screen.getByRole("combobox", {
        name: /Structured history retention/i,
      }),
      "365",
    );
    await user.click(screen.getByRole("button", { name: "Save settings" }));

    const expected: Settings = {
      language: "en-US",
      login_autostart: true,
      notify_max_n: null,
      retention_days: 365,
      system_notifications: false,
    };
    await waitFor(() => {
      expect(replace).toHaveBeenCalledWith(expected, "7");
    });
    expect(
      await screen.findByText("Settings saved and applied."),
    ).toBeInTheDocument();
  });

  it("blocks an invalid threshold locally without sending a partial update", async () => {
    const replace = vi.fn<SettingsApi["replace"]>();
    const user = userEvent.setup();
    render(
      <SettingsView
        api={apiWith(() => Promise.resolve(initialDocument), replace)}
        locale="en-US"
      />,
    );

    const threshold = await screen.findByRole("spinbutton", {
      name: "Maximum n",
    });
    await user.clear(threshold);
    await user.click(screen.getByRole("button", { name: "Save settings" }));

    expect(replace).not.toHaveBeenCalled();
    expect(screen.getByText("Enter a value.")).toBeInTheDocument();
    expect(threshold).toHaveAttribute("aria-invalid", "true");
  });

  it("renders structured server validation next to its setting", async () => {
    const replace = vi.fn<SettingsApi["replace"]>().mockRejectedValue(
      new ApiClientError("validation_failed", {
        issues: [
          {
            field: "login_autostart",
            reason: "unsupported",
          },
          {
            field: "notify_max_n",
            reason: "out_of_range",
            minimum: 1,
            maximum: 2,
          },
        ],
      }),
    );
    const user = userEvent.setup();
    render(
      <SettingsView
        api={apiWith(() => Promise.resolve(initialDocument), replace)}
        locale="en-US"
      />,
    );

    const threshold = await screen.findByRole("spinbutton", {
      name: "Maximum n",
    });
    await user.clear(threshold);
    await user.type(threshold, "3");
    await user.click(screen.getByRole("button", { name: "Save settings" }));

    expect(
      await screen.findByText("Enter a value from 1 to 2."),
    ).toBeInTheDocument();
    expect(threshold).toHaveAttribute("aria-invalid", "true");
    expect(
      screen.getByRole("checkbox", { name: /Start at login/i }),
    ).toHaveAttribute("aria-invalid", "true");
    expect(screen.getByText("Select a supported value.")).toBeInTheDocument();
    expect(screen.getByText("Settings were not saved")).toBeInTheDocument();
  });

  it("requires an explicit reload after a revision conflict", async () => {
    const latestDocument = documentWith(
      { ...initialSettings, retention_days: 30 },
      "9",
    );
    const get = vi
      .fn<SettingsApi["get"]>()
      .mockResolvedValueOnce(initialDocument)
      .mockResolvedValueOnce(latestDocument);
    const replace = vi
      .fn<SettingsApi["replace"]>()
      .mockRejectedValue(new ApiClientError("revision_conflict", {}));
    const user = userEvent.setup();
    render(<SettingsView api={apiWith(get, replace)} locale="en-US" />);

    const retention = await screen.findByRole("combobox", {
      name: /Structured history retention/i,
    });
    await user.selectOptions(retention, "365");
    await user.click(screen.getByRole("button", { name: "Save settings" }));

    expect(
      await screen.findByText("A newer settings revision is available"),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Reload settings" }));
    await waitFor(() => {
      expect(get).toHaveBeenCalledTimes(2);
      expect(
        screen.getByRole("combobox", {
          name: /Structured history retention/i,
        }),
      ).toHaveValue("30");
    });
  });

  it("switches the visible locale after the saved document confirms it", async () => {
    const replace = vi
      .fn<SettingsApi["replace"]>()
      .mockImplementation((settings) =>
        Promise.resolve(documentWith(settings)),
      );
    const user = userEvent.setup();
    render(
      <SettingsView
        api={apiWith(() => Promise.resolve(initialDocument), replace)}
        locale="en-US"
      />,
    );

    await user.selectOptions(
      await screen.findByRole("combobox", { name: /^Language/i }),
      "zh-CN",
    );
    await user.click(screen.getByRole("button", { name: "Save settings" }));

    expect(
      await screen.findByRole("heading", { name: "设置" }),
    ).toBeInTheDocument();
    expect(screen.getByText("设置已保存并应用。")).toBeInTheDocument();
  });

  it("preserves an unsaved draft when a newer settings event arrives", async () => {
    const onSnapshotComplete = vi.fn();
    const api = apiWith(() => Promise.resolve(initialDocument));
    const user = userEvent.setup();
    const { rerender } = render(
      <SettingsView
        api={api}
        locale="en-US"
        synchronization={{
          epoch: 1,
          events: [],
          onSnapshotComplete,
        }}
      />,
    );
    const retention = await screen.findByRole("combobox", {
      name: /Structured history retention/i,
    });
    await user.selectOptions(retention, "365");

    const incoming = documentWith(
      { ...initialSettings, system_notifications: false },
      "8",
    );
    rerender(
      <SettingsView
        api={api}
        locale="en-US"
        synchronization={{
          epoch: 1,
          events: [
            {
              deliveryId: 1,
              event: {
                event_id: "settings-event-8",
                emitted_at: "2026-07-15T04:05:01Z",
                document: incoming,
              },
            },
          ],
          onSnapshotComplete,
        }}
      />,
    );

    expect(
      screen.getByRole("combobox", {
        name: /Structured history retention/i,
      }),
    ).toHaveValue("365");
    expect(
      screen.getByText("A newer settings revision is available"),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Save settings" }),
    ).toBeDisabled();
  });
});
