import { ApiClientError } from "../api/client";
import type { ApiErrorCode, ErrorParameters } from "../api/types";

export interface DisplayError {
  code: ApiErrorCode;
  params: ErrorParameters;
}

export function toDisplayError(error: unknown): DisplayError {
  if (error instanceof ApiClientError) {
    return { code: error.code, params: error.params };
  }
  return { code: "internal_unavailable", params: {} };
}

export function errorTranslationKey(code: ApiErrorCode): `api.${ApiErrorCode}` {
  return `api.${code}`;
}
