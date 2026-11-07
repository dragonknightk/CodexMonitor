# ADR 0006: Typed privacy and action registries

## Status

Accepted on 2026-07-15.

## Context

OTLP inputs can contain arbitrary maps, content and identifiers, while durable
actions can cause operating-system effects. Persisting generic decoded data or
dispatching string-selected effects would make privacy and permission review
open-ended and allow unknown fields or action types to cross trusted boundaries.

## Decision

Use closed, typed registries at both boundaries. A telemetry field may enter the
domain only through a registered source mapping, and only field IDs listed in the
reviewed persistence manifest may enter SQLite. Raw request bodies, arbitrary
attribute maps, prompts, responses, reasoning content, paths and account or
authentication data never cross that boundary. Effectful work resolves through
`RegisteredActionRegistry`, including an exact action type, parameter-schema
version, typed parameters, required capability and stored permission. Unknown
fields, action types, schema versions and dynamic parameters fail closed without a
fallback executor.

## Consequences

- Privacy review has finite machine-readable authorities and generated evidence
  instead of relying on call-site convention.
- Adding a persisted telemetry field requires catalog, persistence-manifest,
  storage and generated-reference changes that agree exactly.
- Adding or changing an action requires a registered typed definition, a new schema
  version when semantics change, bounded parameters and an explicit host executor.
- Generic payload capture and ad hoc shell, plugin or string-dispatched actions are
  outside the architecture.
