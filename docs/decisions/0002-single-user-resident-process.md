# ADR 0002: Single normal-user resident process

## Status

Accepted on 2026-07-15.

## Context

Telemetry ingestion, durable rule work, the Dashboard and native actions need one
owner for current-user state. A system service or several cooperating processes
would add privilege boundaries, inter-process protocols and competing lifecycle
owners without serving the v1 single-user desktop use case.

## Decision

Run CodexMonitor as one normal-user resident process in the interactive desktop
session. Manual launch and `--background` start the same host and engine
composition. A validated second launch forwards the user-visible open operation to
the resident instance rather than starting another engine. The process owns only
the current user's storage and operating-system registrations and never requires
administrator or system-service privileges.

## Consequences

- One process owns the listeners, SQLite writer, workers, tray and native action
  adapters, so startup and shutdown have a single lifecycle boundary.
- Monitoring stops when that user process exits; there is no hidden machine-wide
  service continuing outside the interactive session.
- Different operating-system users have independent processes, credentials and
  storage.
- A future multi-user daemon or split-process architecture would require a new ADR
  covering authentication, IPC, installation and upgrade semantics.
