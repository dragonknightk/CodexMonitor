# ADR 0001: Rust, Tauri 2 and React stack

## Status

Accepted on 2026-07-15.

## Context

CodexMonitor needs a long-running local engine, a small cross-platform desktop
host, native tray and notification integrations, and a typed Dashboard. The
runtime must enforce privacy and lifecycle rules without depending on an
interpreter installed on the user's machine. The user interface should remain
usable in a normal browser and share a contract with any future embedded view.

## Decision

Implement domain policy, runtime adapters and the desktop host in Rust. Use Tauri
2 as the native application shell and React with TypeScript for the Dashboard.
Keep domain policy in `codex-monitor-core`, runtime and protocol adapters in
`codex-monitor-engine`, and operating-system effects in the Tauri host. Tauri is
the lifecycle and platform boundary; it is not a second application-service
layer. The production application does not ship or invoke a Python runtime.

## Consequences

- Domain and runtime invariants are compiled into the only production binary.
- The Dashboard can use generated OpenAPI types while the Rust engine remains the
  authority for runtime behavior.
- Native packaging must build and test the Rust/Tauri targets for each supported
  platform, and frontend work requires the pinned Node.js and pnpm toolchain.
- A platform effect or dependency that cannot fit these module boundaries needs a
  new architecture decision instead of a parallel runtime path.
