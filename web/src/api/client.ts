import createClient from "openapi-fetch";

import type { paths } from "./schema";
import type {
  ApiErrorCode,
  ApiErrorResponse,
  Capabilities,
  CurrentSession,
  ErrorParameters,
  HealthSnapshot,
  PageMetadata,
} from "./types";

export const CSRF_HEADER_NAME = "X-CodexMonitor-CSRF";
const unsafeMethods = new Set(["DELETE", "PATCH", "POST", "PUT"]);

let currentCsrfProof: string | null = null;

export class ApiClientError extends Error {
  readonly code: ApiErrorCode;
  readonly params: ErrorParameters;

  constructor(code: ApiErrorCode, params: ErrorParameters) {
    super(code);
    this.name = "ApiClientError";
    this.code = code;
    this.params = params;
  }
}

function isErrorResponse(value: unknown): value is ApiErrorResponse {
  if (typeof value !== "object" || value === null || !("error" in value)) {
    return false;
  }

  const error = value.error;
  return (
    typeof error === "object" &&
    error !== null &&
    "code" in error &&
    typeof error.code === "string" &&
    "params" in error &&
    typeof error.params === "object" &&
    error.params !== null
  );
}

function isPageMetadata(value: unknown): value is PageMetadata {
  if (
    typeof value !== "object" ||
    value === null ||
    !("limit" in value) ||
    typeof value.limit !== "number" ||
    !Number.isInteger(value.limit) ||
    value.limit < 1 ||
    value.limit > 100 ||
    !("has_more" in value) ||
    typeof value.has_more !== "boolean" ||
    !("next_cursor" in value)
  ) {
    return false;
  }
  return value.has_more
    ? typeof value.next_cursor === "string" && value.next_cursor.length > 0
    : value.next_cursor === null;
}

function requirePageMetadata<T extends { items: unknown[]; page: unknown }>(
  value: T,
): { items: T["items"]; page: PageMetadata } {
  if (!isPageMetadata(value.page)) {
    throw new ApiClientError("internal_unavailable", {});
  }
  return { items: value.items, page: value.page };
}

function dashboardOrigin() {
  const location: unknown = Reflect.get(globalThis, "location");
  if (
    typeof location === "object" &&
    location !== null &&
    "origin" in location &&
    typeof location.origin === "string"
  ) {
    return location.origin;
  }
  return "http://127.0.0.1";
}

async function sameOriginFetch(request: Request): Promise<Response> {
  const requestUrl = new URL(request.url, dashboardOrigin());
  if (requestUrl.origin !== dashboardOrigin()) {
    throw new ApiClientError("forbidden_origin", {});
  }

  const headers = new Headers(request.headers);
  if (unsafeMethods.has(request.method.toUpperCase())) {
    if (currentCsrfProof === null) {
      throw new ApiClientError("csrf_invalid", {});
    }
    headers.set(CSRF_HEADER_NAME, currentCsrfProof);
  }

  return fetch(
    new Request(request, {
      credentials: "same-origin",
      headers,
    }),
  );
}

export const dashboardApi = createClient<paths>({
  baseUrl: dashboardOrigin(),
  credentials: "same-origin",
  fetch: sameOriginFetch,
});

interface Result<T> {
  data?: T;
  error?: unknown;
  response: Response;
}

function unwrap<T>({ data, error }: Result<T>): T {
  if (data !== undefined) {
    return data;
  }
  if (isErrorResponse(error)) {
    throw new ApiClientError(error.error.code, error.error.params);
  }
  throw new ApiClientError("internal_unavailable", {});
}

export async function requestJson<T>(request: Promise<Result<T>>): Promise<T> {
  try {
    return unwrap(await request);
  } catch (error: unknown) {
    if (error instanceof ApiClientError) {
      throw error;
    }
    throw new ApiClientError("internal_unavailable", {});
  }
}

export async function requestPage<
  T extends { items: unknown[]; page: unknown },
>(
  request: Promise<Result<T>>,
): Promise<{
  items: T["items"];
  page: PageMetadata;
}> {
  return requirePageMetadata(await requestJson(request));
}

export function requireCsrfProof(): string {
  if (currentCsrfProof === null) {
    throw new ApiClientError("csrf_invalid", {});
  }
  return currentCsrfProof;
}

export async function getCsrfProof(): Promise<string> {
  if (currentCsrfProof === null) {
    await getCurrentSession();
  }
  return requireCsrfProof();
}

export function clearSessionMetadata() {
  currentCsrfProof = null;
}

export async function getCurrentSession(): Promise<CurrentSession> {
  const session = await requestJson(
    dashboardApi.GET("/api/v1/session", { credentials: "same-origin" }),
  );
  currentCsrfProof = session.csrf.proof;
  return session;
}

export function getHealth(): Promise<HealthSnapshot> {
  return requestJson(dashboardApi.GET("/api/v1/health"));
}

export function getCapabilities(): Promise<Capabilities> {
  return requestJson(dashboardApi.GET("/api/v1/capabilities"));
}
