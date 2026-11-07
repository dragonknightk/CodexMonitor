# ADR 0003: Separate OTLP and Dashboard listeners

## Status

Accepted on 2026-07-15.

## Context

OTLP ingestion and the Dashboard have different protocols, authentication rules,
failure modes and port requirements. Combining them behind one HTTP listener
would couple receiver availability to browser-session security and make a fixed
telemetry endpoint share lifecycle with a user-facing API.

## Decision

The engine owns two independent loopback HTTP listeners. The OTLP receiver binds
exactly `127.0.0.1:4318`. The authenticated Dashboard listener prefers
`127.0.0.1:4319` and may choose another loopback port when that port is occupied.
They do not share a router, socket or externally reachable interface. Each has its
own readiness, error state and shutdown stage, and CodexMonitor has no default
outbound telemetry path.

## Consequences

- An OTLP bind conflict can be reported and retried without changing the
  Dashboard's authentication boundary.
- Browser routes cannot accidentally inherit unauthenticated ingestion behavior,
  and receiver requests cannot consume Dashboard sessions.
- Startup and shutdown must compose, supervise and close two listener lifecycles.
- Local integrations must target the fixed OTLP address, while browser clients use
  the active Dashboard origin issued by the resident process.
