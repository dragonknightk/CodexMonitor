import { expect } from "@playwright/test";
import type { Page } from "@playwright/test";

import type { Locale, Settings } from "../../src/api/types";

export interface DashboardCopy {
  alertsHeading: string;
  applyFilters: string;
  callsHeading: string;
  clearFilters: string;
  conversationFilter: string;
  emptyCalls: string;
  filterForm: string;
  fromFilter: string;
  invalidTimeRange: string;
  languageField: string;
  modelFilter: string;
  pagination: string;
  saveSettings: string;
  savedSettings: string;
  settingsForm: string;
  settingsHeading: string;
  sourceFilter: string;
  sourcesHeading: string;
  tableCaption: string;
  toFilter: string;
}

export const localeCopy: Record<Locale, DashboardCopy> = {
  "en-US": {
    alertsHeading: "Reasoning alerts",
    applyFilters: "Apply filters",
    callsHeading: "Model calls",
    clearFilters: "Clear filters",
    conversationFilter: "Conversation / thread",
    emptyCalls: "No model calls yet",
    filterForm: "Filter model calls",
    fromFilter: "From",
    invalidTimeRange: "The start time must be earlier than the end time.",
    languageField: "Language",
    modelFilter: "Model",
    pagination: "Model-call pages",
    saveSettings: "Save settings",
    savedSettings: "Settings saved and applied.",
    settingsForm: "CodexMonitor settings",
    settingsHeading: "Settings",
    sourceFilter: "Source",
    sourcesHeading: "Sources",
    tableCaption: "Recent model calls",
    toFilter: "To (exclusive)",
  },
  "zh-CN": {
    alertsHeading: "推理告警",
    applyFilters: "应用筛选",
    callsHeading: "模型调用",
    clearFilters: "清除筛选",
    conversationFilter: "会话 / 线程",
    emptyCalls: "暂无模型调用",
    filterForm: "筛选模型调用",
    fromFilter: "开始时间",
    invalidTimeRange: "开始时间必须早于结束时间。",
    languageField: "语言",
    modelFilter: "模型",
    pagination: "模型调用分页",
    saveSettings: "保存设置",
    savedSettings: "设置已保存并应用。",
    settingsForm: "CodexMonitor 设置",
    settingsHeading: "设置",
    sourceFilter: "来源",
    sourcesHeading: "遥测来源",
    tableCaption: "最近的模型调用",
    toFilter: "结束时间（不含）",
  },
};

interface SettingsUpdate {
  csrfProof: string | undefined;
  ifMatch: string | undefined;
  settings: Settings;
}

export interface DashboardHarness {
  modelCallRequestUrls: string[];
  settingsUpdates: SettingsUpdate[];
}

const csrfProof = "playwright_csrf_proof_0123456789abcdef";

function isSettings(value: unknown): value is Settings {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const candidate = value as Record<string, unknown>;
  return (
    typeof candidate.login_autostart === "boolean" &&
    typeof candidate.system_notifications === "boolean" &&
    (candidate.notify_max_n === null ||
      typeof candidate.notify_max_n === "number") &&
    (candidate.retention_days === null ||
      candidate.retention_days === 30 ||
      candidate.retention_days === 90 ||
      candidate.retention_days === 365) &&
    (candidate.language === "en-US" || candidate.language === "zh-CN")
  );
}

function settingsDocument(settings: Settings, revision: string) {
  return {
    applied_at: "2026-07-15T04:00:00Z",
    revision,
    settings,
  };
}

export function projectLocale(projectName: string): Locale {
  return projectName.endsWith("zh-CN") ? "zh-CN" : "en-US";
}

export async function installDashboardMock(
  page: Page,
  initialLocale: Locale,
): Promise<DashboardHarness> {
  await page.addInitScript(() => {
    class StableEventSource extends EventTarget {
      static readonly CLOSED = 2;
      static readonly CONNECTING = 0;
      static readonly OPEN = 1;

      readonly url: string;
      readonly withCredentials = true;
      readyState = StableEventSource.CONNECTING;
      onerror: ((event: Event) => void) | null = null;
      onmessage: ((event: MessageEvent) => void) | null = null;
      onopen: ((event: Event) => void) | null = null;

      constructor(url: string | URL) {
        super();
        this.url = String(url);
        queueMicrotask(() => {
          if (this.readyState !== StableEventSource.CONNECTING) {
            return;
          }
          this.readyState = StableEventSource.OPEN;
          const event = new Event("open");
          this.onopen?.(event);
          this.dispatchEvent(event);
        });
      }

      close() {
        this.readyState = StableEventSource.CLOSED;
      }
    }

    Object.defineProperty(window, "EventSource", {
      configurable: true,
      value: StableEventSource,
    });
  });

  const harness: DashboardHarness = {
    modelCallRequestUrls: [],
    settingsUpdates: [],
  };
  let revision = "7";
  let settings: Settings = {
    language: initialLocale,
    login_autostart: false,
    notify_max_n: 1,
    retention_days: 90,
    system_notifications: false,
  };

  const modelCall = {
    alert_count: 1,
    alert_state: "matched",
    client: "codex-desktop",
    conversation: {
      id: "019b7ba7-2d7e-7a18-bf2d-a5c3691a9201",
      resolved_display_name: "Synthetic conversation",
      source_id: "synthetic-thread",
      thread_id: null,
    },
    event_at: "2026-07-15T04:03:00Z",
    field_sources: [],
    id: "019b7ba7-2d7e-7a18-bf2d-a5c3691a9301",
    model: "gpt-5.6-live",
    reasoning_effort: "high",
    source_ids: ["otlp-logs"],
    surface: "desktop",
    temporal_class: "realtime",
    tokens: {
      cached_input: 20,
      input: 120,
      output: 40,
      reasoning: 16,
      total: 160,
    },
    turn_id: null,
  };

  await page.route("**/api/v1/health", async (route) => {
    await route.fulfill({
      json: {
        api_version: "v1",
        checked_at: "2026-07-15T04:00:00Z",
        dashboard_listener: {
          host: "127.0.0.1",
          port: 4319,
          state: "bound",
        },
        status: "ready",
        stream_cursor: "playwright-stream-cursor-1",
      },
    });
  });
  await page.route("**/api/v1/model-calls**", async (route) => {
    harness.modelCallRequestUrls.push(route.request().url());
    const requestedModel = new URL(route.request().url()).searchParams.get(
      "model",
    );
    const matches =
      requestedModel === null ||
      requestedModel.length === 0 ||
      requestedModel === modelCall.model;
    await route.fulfill({
      json: {
        items: matches ? [modelCall] : [],
        page: { has_more: false, limit: 25, next_cursor: null },
      },
    });
  });
  await page.route("**/api/v1/summaries**", async (route) => {
    const requestedModel = new URL(route.request().url()).searchParams.get(
      "model",
    );
    const matches =
      requestedModel === null ||
      requestedModel.length === 0 ||
      requestedModel === modelCall.model;
    const count = matches ? 1 : 0;
    const aggregate = (value: number) => ({
      complete: true,
      known_call_count: count,
      total_call_count: count,
      value: matches ? value : 0,
    });
    await route.fulfill({
      json: {
        alert_count: count,
        cached_input: aggregate(20),
        call_count: count,
        input: aggregate(120),
        output: aggregate(40),
        reasoning: aggregate(16),
        total: aggregate(160),
      },
    });
  });
  await page.route("**/api/v1/sources", async (route) => {
    await route.fulfill({
      json: {
        items: [
          {
            last_accepted_at: null,
            last_error_code: "otlp_bind_failed",
            last_error_occurred_at: "2026-07-15T04:02:00Z",
            receiver: {
              bind_state: "conflict",
              host: "127.0.0.1",
              port: 4318,
              retry_supported: true,
            },
            source_contract_version: 1,
            source_id: "otlp-logs",
            status: "unavailable",
            supported_fields: ["model_call.reasoning_output_tokens"],
            updated_at: "2026-07-15T04:02:01Z",
          },
        ],
      },
    });
  });
  await page.route("**/api/v1/alerts**", async (route) => {
    await route.fulfill({
      json: {
        items: [
          {
            action: {
              action_type: "system_notification",
              completed_at: "2026-07-15T04:01:02Z",
              created_at: "2026-07-15T04:01:02Z",
              error_code: null,
              not_before: "2026-07-15T04:01:02Z",
              started_at: null,
              state: "suppressed",
              suppression_reason: "notifications_disabled",
            },
            conversation_id: null,
            created_at: "2026-07-15T04:01:02Z",
            evidence: {
              event_at: "2026-07-15T04:01:00Z",
              observed_at: "2026-07-15T04:01:01Z",
              pattern_n: 1,
              quality: "direct",
              reasoning_output_tokens: 516,
              source_contract_version: 1,
              source_id: "otlp-logs",
            },
            id: "019b7ba7-2d7e-7a18-bf2d-a5c3691a9001",
            model_call_id: "019b7ba7-2d7e-7a18-bf2d-a5c3691a9002",
            rule_id: "suspected-reasoning-limit-pattern",
            rule_version: 3,
          },
        ],
        page: { has_more: false, limit: 10, next_cursor: null },
      },
    });
  });
  await page.route("**/api/v1/session", async (route) => {
    await route.fulfill({
      json: {
        authenticated: true,
        csrf: {
          header_name: "X-CodexMonitor-CSRF",
          proof: csrfProof,
        },
        expires_at: "2026-07-15T05:00:00Z",
      },
    });
  });
  await page.route("**/api/v1/settings", async (route) => {
    if (route.request().method() === "PUT") {
      const body: unknown = route.request().postDataJSON();
      if (!isSettings(body)) {
        throw new Error("Playwright received an invalid settings document");
      }
      const headers = route.request().headers();
      harness.settingsUpdates.push({
        csrfProof: headers["x-codexmonitor-csrf"],
        ifMatch: headers["if-match"],
        settings: body,
      });
      settings = body;
      revision = String(Number(revision) + 1);
    }
    await route.fulfill({ json: settingsDocument(settings, revision) });
  });
  return harness;
}

export async function openDashboard(page: Page, locale: Locale): Promise<void> {
  await page.goto("/");
  await expect(
    page.getByRole("heading", {
      exact: true,
      level: 2,
      name: localeCopy[locale].callsHeading,
    }),
  ).toBeVisible();
  await expect(page.getByText("gpt-5.6-live", { exact: true })).toHaveCount(1);
  await expect(
    page.getByRole("form", {
      exact: true,
      includeHidden: true,
      name: localeCopy[locale].settingsForm,
    }),
  ).toBeAttached();
}
