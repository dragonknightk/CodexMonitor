# ADR 0004: Browser-first HTTP Dashboard

## Status

Accepted on 2026-07-15.

## Context

Tauri can expose application behavior through a native WebView and JavaScript IPC,
but that would create a privileged UI-only protocol beside the HTTP API. A local
monitoring Dashboard is easier to test, inspect and reuse when its complete
contract works in an ordinary browser.

## Decision

Serve the React Dashboard and all v1 business operations from the loopback
Dashboard listener through HTTP REST and SSE. The Tauri host configures no native
window and opens the default browser with a short-lived one-time bootstrap URL.
The Dashboard does not depend on the Tauri JavaScript bridge. Any future embedded
WebView must load the same local origin and use the same authenticated HTTP
contract; it may not introduce a second business IPC path.

## Consequences

- Browser and future embedded clients exercise one OpenAPI-governed contract.
- The HTTP service must provide bootstrap credentials, bounded sessions, CSRF
  protection, exact host/origin validation and explicit SSE resynchronization.
- Native effects remain behind typed engine-to-host ports instead of being callable
  directly from frontend JavaScript.
- Opening the system browser is a host responsibility, while Dashboard behavior can
  be developed and tested without a native window.
