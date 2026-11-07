import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { clearSessionMetadata, getCurrentSession } from "../api/client";
import type { Settings } from "../api/types";
import { alertsApi } from "./alerts";
import { callsApi } from "./calls";
import { conversationsApi } from "./conversations";
import { settingsApi } from "./settings";
import { sourcesApi } from "./sources";

interface RecordedRequest {
  request: Request;
  body: string;
}

const csrfProof = "synthetic_csrf_proof_0123456789abcdef0123456789";
const timestamp = "2026-01-01T00:00:00Z";
const settings: Settings = {
  login_autostart: false,
  system_notifications: true,
  notify_max_n: 1,
  retention_days: 90,
  language: "en-US",
};

let recordedRequests: RecordedRequest[] = [];

function jsonResponse(value: unknown, status = 200): Response {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function page(url: URL) {
  return {
    items: [],
    page: {
      limit: Number(url.searchParams.get("limit") ?? 25),
      has_more: false,
      next_cursor: null,
    },
  };
}

async function fakeFetch(
  input: RequestInfo | URL,
  init?: RequestInit,
): Promise<Response> {
  const request = input instanceof Request ? input : new Request(input, init);
  recordedRequests.push({
    request,
    body: await request.clone().text(),
  });
  const url = new URL(request.url);
  const operation = `${request.method} ${url.pathname}`;

  switch (operation) {
    case "GET /api/v1/session":
      return jsonResponse({
        authenticated: true,
        csrf: {
          header_name: "X-CodexMonitor-CSRF",
          proof: csrfProof,
        },
        expires_at: timestamp,
      });
    case "GET /api/v1/model-calls":
    case "GET /api/v1/conversations":
    case "GET /api/v1/alerts":
      return jsonResponse(page(url));
    case "GET /api/v1/summaries": {
      const aggregate = {
        value: 0,
        known_call_count: 0,
        total_call_count: 0,
        complete: true,
      };
      return jsonResponse({
        call_count: 0,
        alert_count: 0,
        input: aggregate,
        cached_input: aggregate,
        output: aggregate,
        reasoning: aggregate,
        total: aggregate,
      });
    }
    case "GET /api/v1/sources":
      return jsonResponse({ items: [] });
    case "POST /api/v1/sources/otlp-logs/retry":
      return jsonResponse(
        {
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
          last_error_occurred_at: timestamp,
          updated_at: timestamp,
          supported_fields: [],
        },
        202,
      );
    case "GET /api/v1/settings":
    case "PUT /api/v1/settings":
      return jsonResponse({
        revision: "7",
        settings,
        applied_at: timestamp,
      });
    default:
      return jsonResponse(
        { error: { code: "internal_unavailable", params: {} } },
        503,
      );
  }
}

function recordedRequest(method: string, path: string): RecordedRequest {
  const result = recordedRequests.find((entry) => {
    const url = new URL(entry.request.url);
    return entry.request.method === method && url.pathname === path;
  });
  if (result === undefined) {
    throw new Error(`missing recorded request: ${method} ${path}`);
  }
  return result;
}

beforeEach(() => {
  clearSessionMetadata();
  recordedRequests = [];
  vi.stubGlobal("fetch", vi.fn(fakeFetch));
});

afterEach(() => {
  clearSessionMetadata();
  vi.unstubAllGlobals();
});

describe("feature API modules", () => {
  it("owns call history and summary operations", async () => {
    await callsApi.list({ limit: 2, model: "gpt-5.5", sort: "event_at_asc" });
    await callsApi.summarize({ model: "gpt-5.5" });

    const listUrl = new URL(
      recordedRequest("GET", "/api/v1/model-calls").request.url,
    );
    expect(listUrl.searchParams.get("limit")).toBe("2");
    expect(listUrl.searchParams.get("model")).toBe("gpt-5.5");
    expect(listUrl.searchParams.get("sort")).toBe("event_at_asc");
    const summaryUrl = new URL(
      recordedRequest("GET", "/api/v1/summaries").request.url,
    );
    expect(summaryUrl.searchParams.get("model")).toBe("gpt-5.5");
  });

  it("owns conversation pagination", async () => {
    await conversationsApi.list({
      limit: 3,
      query: "Mission",
      source_id: "otlp-logs",
    });

    const url = new URL(
      recordedRequest("GET", "/api/v1/conversations").request.url,
    );
    expect(url.searchParams.get("limit")).toBe("3");
    expect(url.searchParams.get("query")).toBe("Mission");
    expect(url.searchParams.get("source_id")).toBe("otlp-logs");
  });

  it("owns alert pagination", async () => {
    await alertsApi.list({ limit: 4, action_state: "pending" });

    const url = new URL(recordedRequest("GET", "/api/v1/alerts").request.url);
    expect(url.searchParams.get("limit")).toBe("4");
    expect(url.searchParams.get("action_state")).toBe("pending");
  });

  it("owns source reads and CSRF-protected retries", async () => {
    await getCurrentSession();
    await sourcesApi.list();
    await sourcesApi.retry("otlp-logs");

    const retry = recordedRequest("POST", "/api/v1/sources/otlp-logs/retry");
    expect(retry.request.credentials).toBe("same-origin");
    expect(retry.request.headers.get("X-CodexMonitor-CSRF")).toBe(csrfProof);
    expect(retry.body).toBe("");
  });

  it("lazily refreshes CSRF metadata before the first source mutation", async () => {
    clearSessionMetadata();
    await sourcesApi.retry("otlp-logs");

    expect(recordedRequest("GET", "/api/v1/session")).toBeDefined();
    const retry = recordedRequest("POST", "/api/v1/sources/otlp-logs/retry");
    expect(retry.request.headers.get("X-CodexMonitor-CSRF")).toBe(csrfProof);
  });

  it("owns settings reads and revision-guarded replacement", async () => {
    await getCurrentSession();
    await settingsApi.get();
    await settingsApi.replace(settings, "7");

    const replace = recordedRequest("PUT", "/api/v1/settings");
    expect(replace.request.credentials).toBe("same-origin");
    expect(replace.request.headers.get("X-CodexMonitor-CSRF")).toBe(csrfProof);
    expect(replace.request.headers.get("If-Match")).toBe('"7"');
    expect(JSON.parse(replace.body)).toEqual(settings);
  });

  it("lazily refreshes CSRF metadata before the first settings mutation", async () => {
    clearSessionMetadata();
    await settingsApi.replace(settings, "7");

    expect(recordedRequest("GET", "/api/v1/session")).toBeDefined();
    const replace = recordedRequest("PUT", "/api/v1/settings");
    expect(replace.request.headers.get("X-CodexMonitor-CSRF")).toBe(csrfProof);
    expect(replace.request.headers.get("If-Match")).toBe('"7"');
  });
});
