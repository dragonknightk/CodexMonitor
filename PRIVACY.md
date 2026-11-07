# Privacy

[中文权威版本](PRIVACY.zh-CN.md)；本文件是英文翻译。

CodexMonitor applies data minimization before persistence. This document
describes the local data boundary for the packaged Rust/Tauri application.

## Network boundary

- The OTLP receiver is fixed at `127.0.0.1:4318` and exposes
  `/v1/logs` for local Codex telemetry.
- The Dashboard and `/api/v1` listen only on loopback. The Dashboard prefers
  port `4319` and may use another available loopback port.
- CodexMonitor has no default remote telemetry, analytics, crash upload,
  monitoring-data forwarding, or update-check request.
- Requests to an OpenAI-hosted model still travel to OpenAI. That model path is
  separate from the local Codex OTel exporter.

Loopback reduces network exposure, but it is not a security boundary between
processes running as the same operating-system user. Malware running as your
user can generally read or modify your local database and settings directly.

## Data received but not stored

Codex OTel events can contain prompts, responses, tool parameters, commands,
paths, tool output, account details, host details, and new fields added by later
versions. CodexMonitor does not store a generic attribute map or a filtered copy
of the original event.

Raw request bytes and decoded generic OTLP messages exist only during bounded
in-memory request processing. Typed extraction reads registered event,
attribute, and `response.usage` paths. Everything not registered in the reviewed
field catalog is discarded before the storage boundary, including:

- raw OTLP bodies, raw JSON, and payload excerpts;
- prompts, messages, response text, and session titles;
- tool parameters, commands, tool output, and unrestricted URLs;
- reasoning summaries, encrypted reasoning, and other reasoning content;
- workspace, sandbox, approval, executable, and file paths;
- email addresses, account identifiers, API keys, cookies, and authentication
  tokens.

Decoder and processing errors record only safe codes and bounded facts such as
encoding and size class. They do not include payload excerpts.

## Data stored locally

Only fields registered in the telemetry field catalog may be persisted. Where
available, this includes validated token counts, model, reasoning effort,
timestamps, event/source/client/surface metadata, field provenance, source
health, rule evidence, action state, settings, and migration audit facts.

Conversation and thread identifiers may be stored for local correlation and
filtering. They are pseudonymous identifiers and should still be treated as
sensitive local data. They are excluded from ordinary diagnostics and system
notifications.

`session_index.jsonl` may be read at query time to resolve a display name. The
name can appear in the local Dashboard but is not copied into monitoring tables.
If the index is absent or invalid, the Dashboard uses a shortened identifier.

The application stores its database, settings, and structured logs in the
current user's application data directory:

| Platform | Data directory                                                                                  |
| -------- | ----------------------------------------------------------------------------------------------- |
| Windows  | `%LOCALAPPDATA%\CodexMonitor`                                                                   |
| macOS    | `~/Library/Application Support/CodexMonitor`                                                    |
| Linux    | `$XDG_DATA_HOME/codex-monitor`, or `~/.local/share/codex-monitor` when `XDG_DATA_HOME` is unset |

The application restricts permissions to the current user as far as each
operating system permits.

## Dashboard security

Opening the Dashboard creates a fresh, one-time bootstrap URL. The server
exchanges that token for a per-process `HttpOnly`, `SameSite=Strict` session and
redirects to a clean URL. Reused or expired bootstrap tokens are rejected.

The API validates the loopback host, does not enable permissive CORS, and
requires both the current session and CSRF protection for settings changes.
Never share or paste a bootstrap URL into a bug report even though the token is
short-lived.

## System notifications

System notifications use fixed localized templates. They may contain only the
alert type, an optional model name, the reasoning token count, the computed
pattern number, and a recommendation to review the result. A missing model is
not replaced with a session name or another field. Grouped notifications also
exclude conversation/thread identifiers.

Notifications never contain a prompt, response, reasoning content, session
title, tool data, command, path, account identifier, or authentication material.
The operating system can display the approved title and body in its notification
center or on the lock screen. Disable reasoning notifications in CodexMonitor or
hide notification previews in the operating system if others can see the screen.

Permission, service, and delivery failures are stored as closed safe status
codes. Native error text is not copied into action audit or diagnostics. Alerts
remain in the Dashboard when notifications are disabled or unavailable.

## Legacy database import

The native importer treats the selected legacy database as untrusted read-only
input. It validates the schema and queries only fixed allowlisted structured
fields. Invalid token values are discarded individually. Raw-record tables,
`raw_json`, prompts, tool data, and extra private columns are never queried or
copied.

Import is requested explicitly with
`codex-monitor --import-legacy <absolute-sqlite-path>`. Relative paths are
rejected. If an instance is already resident, the single-instance route forwards
the request to it; no legacy Python runtime is started or retained.

Allowed fields and a path-free import audit are committed atomically to the new
database. The source file remains unchanged. Sanitized audit fingerprints and
row identities make repeated imports idempotent. Imported records are marked as
historical: they cannot create realtime alerts or notifications and cannot enter
the automation pipeline. A successful or already-completed import opens the
local Dashboard.

## Retention and deletion

Structured monitoring history defaults to 90 days. The available retention
choices are 30 days, 90 days, 365 days, or unlimited. Cleanup preserves pending
actions and migration audit records needed for integrity.

Uninstalling CodexMonitor preserves the user data directory by default so an
upgrade or reinstall does not erase history. To remove all local data, first
quit the resident process with the tray Quit action or `--quit`, then follow the
platform-specific commands in [Uninstall and delete data](docs/user/uninstall.md).
Deletion cannot remove copies you created, backups, filesystem snapshots, or
attachments already uploaded elsewhere.

## Codex configuration

Keep this privacy-preserving baseline:

```toml
[analytics]
enabled = false

[otel]
log_user_prompt = false
metrics_exporter = "none"
trace_exporter = "none"
```

`log_user_prompt = false` controls the prompt field exposed by Codex; it cannot
guarantee that every future OTel attribute is harmless. CodexMonitor therefore
still enforces its own persistence allowlist. Do not configure this exporter to
send to a remote destination for CodexMonitor.

## Local files and bug reports

Repository ignore rules exclude SQLite databases, WAL/SHM files, JSONL files,
logs, local settings, environment files, and local build state. Do not bypass
those rules with `git add -f`.

Never upload a production database, local Codex log, session index, bootstrap
URL, or complete application-data directory as a reproduction attachment. A
safe report contains the CodexMonitor version, operating system, package type,
architecture, development/signing label, safe status code, and steps using
synthetic data. See [Troubleshooting](docs/user/troubleshooting.md).
