# CodexMonitor v1 能力追踪

本台账把 `codex-monitor-v1` 的每项 requirement/scenario 映射到当前实现与最强可用证据。它是证据目录，不会把“存在命令或 workflow”写成“本次审计已经执行并通过”。

## 覆盖与状态语义

| Capability                 | Requirements | Scenarios |
| -------------------------- | -----------: | --------: |
| `automation-actions`       |            8 |        11 |
| `documentation-governance` |           10 |        16 |
| `local-dashboard`          |            9 |        16 |
| `localization`             |            8 |        12 |
| `monitoring-data`          |            9 |        17 |
| `privacy-controls`         |           10 |        13 |
| `reasoning-alerts`         |            8 |        16 |
| `resident-monitor-host`    |            7 |        14 |
| `telemetry-ingestion`      |            8 |        16 |
| **合计**                   |       **77** |   **131** |

- **本机自动证据**：repository unit/integration/property/browser/tooling/document gate 已存在。“证据存在”不表示本次文档编辑重跑了完整 suite。
- **Hosted native workflow**：runner 与 workflow 已定义；只有对应 hosted job 成功且保留 evidence 才算实际执行。
- **人工 installed evidence**：必须由人观察安装应用；source inspection、mock 或进程正常退出不能替代。
- **H1 deferred**：accepted Windows portable boundary 之外的 native execution 尚未作为 v1 完成证据。
- **M1 confirmed**：下文记录的可见 Windows notification 已由用户观察确认。

路径前缀：`core/` 表示 `crates/codex-monitor-core/src/`，`storage/` 表示 `crates/codex-monitor-storage/src/`，`telemetry/` 表示 `crates/codex-monitor-telemetry/src/`，`dashboard/` 表示 `crates/codex-monitor-dashboard/src/`，`engine/` 表示 `crates/codex-monitor-engine/src/`，`host/` 表示 `crates/codex-monitor-host/src/`，`desktop/` 表示 `apps/desktop/src-tauri/src/`。

## 跨 capability 证据集

### L1：原生 11-case legacy contract

`contracts/legacy-otlp-v1.json` 与 `contracts/legacy-otlp-v1-disposition.json` 由 `crates/codex-monitor-telemetry/src/legacy_contract.rs` 的测试强制验证。它们检查精确 case coverage、hash、request reconstruction 与 privacy markers，再让全部冻结 case 通过 telemetry decoder、typed extractor、HTTP router 和假的 consumer-owned observation sink。storage 投影与 engine bridge 分别由 storage 测试和产品集成目标验证，从而保持 telemetry crate 不依赖 SQLite。详情见[旧 OTLP 合同切换](legacy-contract-cutover.md)。

### G1：Fixture 与 removed-runtime gates

`scripts/check-repository-fixtures.mjs` 读取 staged Git blobs，拒绝 local-data artifact 并验证 synthetic OTLP contract。`scripts/check-no-legacy-runtime.mjs` 检查 tracked 与 untracked non-ignored candidates，拒绝 Python tree、entry point、shim 或 workflow dispatch。对应 script tests 覆盖 positive/negative 与 index/worktree divergence。

### R1：Settings receiver controls

Dashboard settings 与 source views 使用同一 typed receiver snapshot/retry port。Dashboard HTTP tests、engine bridge tests、frontend receiver-control tests 与 host port-conflict presentation tests 覆盖 authorization、closed status、retry wakeup 和设置文档隔离。

### L2：本机安装 Windows smoke

2026-07-17 的 accepted v1 boundary 是 Windows portable executable、fresh isolated profile 与 copied synthetic legacy database。执行覆盖 manual/background/second-instance、OTLP、SQLite、Dashboard bootstrap/assets、legacy import、rule/action、autostart 和 ordered quit。它不代表 installer upgrade 或其他平台已经执行。

### H1：非 Windows 与 installer upgrade — deferred

Windows installer、macOS x64/arm64 与 Linux x64 workflow、release contract 和 installed-smoke source 已存在，仍需对应 hosted run 才能升级为执行证据。定义矩阵不等于运行矩阵。

### M1：可见 Windows toast — confirmed

2026-07-17 人工确认 installed Windows application 能显示固定 privacy-safe notification copy。自动测试继续验证 typed parameters、模板 allowlist、permission/unavailable/failure state 与 forbidden markers；人工确认只覆盖可见 presentation。

## `automation-actions`

| Requirement                              | 当前实现                                                                    | 证据与状态                                                      |
| ---------------------------------------- | --------------------------------------------------------------------------- | --------------------------------------------------------------- |
| 已登记的类型化 action executor           | `core/action.rs` registry；`engine/actions.rs` validation；desktop executor | Registry/schema/capability mismatch tests；已实现，本机自动证据 |
| Transactional action Outbox              | `storage/action_outbox.rs` 与 rule completion transaction                   | Atomic insert/rollback/replay/migration tests；已实现           |
| 可审计 action lifecycle                  | `pending -> started -> terminal` 与 attempt records                         | Claim/start/finish/fencing/repository tests；已实现             |
| At-most-once system notification attempt | `started` 不返回 pending；startup recovery 标记 indeterminate               | Crash/restart/recovery/idempotence tests；已实现                |
| Privacy-safe system notification         | `SystemNotificationParams` 与固定 host templates                            | Privacy-surface、renderer、forbidden-marker tests；M1 confirmed |
| 未来 effect 需要显式权限                 | Registry capability/permission 与用户授权政策                               | Core/engine rejection tests、action how-to；已实现              |
| 禁止任意命令求值                         | 无 shell/script/string executor 或 fallback                                 | Registry closed-enum tests、privacy/no-legacy gates；已实现     |
| v1 仅有 system notification effect       | 精确单项 action registry                                                    | Registry completeness、host executor contract tests；已实现     |

## `documentation-governance`

| Requirement                               | 当前实现                                                     | 证据与状态                                                |
| ----------------------------------------- | ------------------------------------------------------------ | --------------------------------------------------------- |
| 渐进式文档入口                            | Root README/privacy、`docs/index.md`                         | Document inventory/navigation checks；已实现              |
| Scoped Agent instructions                 | v1 曾使用 scoped files；当前仓库已移除全部 `AGENTS.md`       | Agent file rejection test；v1历史已实现，当前已显式删除   |
| Machine-readable documentation impact map | `documentation-map.yaml` version 2                           | Schema/inventory/generator/mapping tests；已实现          |
| Enforced documentation impact             | PR declaration 与 base-aware checker                         | `read-docs-impact`、CI workflow contract tests；已实现    |
| Generated reference 单一权威              | Field/source YAML、migrations、OpenAPI                       | Drift 与 unsupported-change tests；已实现                 |
| Architecture decision records             | Immutable legacy ADR + Chinese companions + Chinese ADR 0007 | Historical mutation/translation/navigation checks；已实现 |
| 规范与文档治理门禁                        | `pnpm docs:check` 与变更影响校验                             | CI/tool command；已实现                                   |
| Extension workflows                       | `docs/how-to/add-*`                                          | Document map 与 link reachability；已实现                 |
| Language policy                           | 中文 authority、英文用户翻译                                 | Chinese narrative/translation relation tests；已实现      |
| Documentation/fixture privacy             | Git-index fixture gate 与 synthetic-only policy              | G1；已实现                                                |

## `local-dashboard`

| Requirement                      | 当前实现                                                      | 证据与状态                                                  |
| -------------------------------- | ------------------------------------------------------------- | ----------------------------------------------------------- |
| Authenticated loopback Dashboard | `codex-monitor-dashboard` listener/session/security           | Loopback、bootstrap、cookie、Host/Origin/CSRF tests；已实现 |
| Versioned API contract           | `api/openapi.yaml` 与 generated TypeScript                    | OpenAPI/generation contract tests；已实现                   |
| Model-call history               | Dashboard query port、storage projection、React calls feature | Storage query、HTTP response、component tests；已实现       |
| Filtering/pagination             | Typed input、opaque bound cursor、client stack                | Boundary/cursor/sort/filter tests；已实现                   |
| Realtime resynchronization       | Storage change feed 与 SSE control events                     | Replay/overflow/restart/session-eviction tests；已实现      |
| Source/receiver diagnostics      | Closed `SourceHealth`/retry DTO                               | Telemetry receiver、Dashboard、frontend、R1 tests；已实现   |
| Alert history/evidence           | Stored alert provenance 与 action state DTO                   | Storage/engine/frontend tests；已实现                       |
| Settings view                    | Revision CAS、targeted mutations 与 runtime capabilities      | Engine settings、Dashboard HTTP、frontend tests；已实现     |
| Presentation-host independence   | Browser-first HTTP；dormant future-window adapter 共用 origin | ADR 0004、desktop adapter tests、无 Tauri IPC scan；已实现  |

## `localization`

| Requirement                       | 当前实现                                          | 证据与状态                                  |
| --------------------------------- | ------------------------------------------------- | ------------------------------------------- |
| 初始 `en-US`/`zh-CN`              | Frontend feature catalogs 与 native host catalog  | Namespace/visible-text checks；已实现       |
| 初始 locale detection/persistence | Settings initialization                           | Storage/engine initialization tests；已实现 |
| 无重启即时切换                    | Settings replacement 与 live tray/frontend update | Component/Playwright/tray tests；已实现     |
| Feature-based namespaces          | `web/src/locales/*`                               | Catalog parity test；已实现                 |
| Structured backend messages       | Stable codes + frontend closed translation        | Error-code and forbidden-text tests；已实现 |
| Locale-aware formatting           | `Intl` presentation，API 保持 UTC/numeric         | Unit/Playwright tests；已实现               |
| Translation catalog verification  | `scripts/check-localization.mjs`                  | Positive/negative script tests；已实现      |
| Stable identifiers 不翻译         | Model/event/API/config identifiers 原样呈现       | Catalog and UI tests；已实现                |

## `monitoring-data`

| Requirement                            | 当前实现                                             | 证据与状态                                             |
| -------------------------------------- | ---------------------------------------------------- | ------------------------------------------------------ |
| Canonical conversation/turn/call model | `core/domain.rs`、`core/observation.rs`              | Constructor/invariant/serialization tests；已实现      |
| Field provenance/quality               | Typed provenance、quality/source precedence          | Matrix/property/replay-order tests；已实现             |
| Deterministic identity/dedup           | Versioned source identity/fingerprint                | Retransmission/concurrency/restart tests；已实现       |
| Local SQLite persistence               | `codex-monitor-storage` writer/repository/projection | 69 项 storage tests；已实现                            |
| Append-only migrations                 | Embedded contiguous ledger/checksum                  | Exact blob/checksum/schema tests；已实现               |
| Structured retention                   | Typed 30/90/365/unlimited policy                     | Bounded cleanup/restart/rollback tests；已实现         |
| Safe legacy import                     | Hardened read-only detector/importer                 | L1、G1、legacy schema/privacy tests；已实现            |
| Safe session label resolution          | Telemetry session index memory-only resolver         | Bounds/refresh/malformed/non-persistence tests；已实现 |
| UTC/numeric integrity                  | `UtcTimestamp`、`TokenCount` bounds                  | Core/storage/telemetry boundary tests；已实现          |

## `privacy-controls`

| Requirement                        | 当前实现                                               | 证据与状态                                              |
| ---------------------------------- | ------------------------------------------------------ | ------------------------------------------------------- |
| Local-only network boundary        | 两个 loopback listeners，无 outbound telemetry         | Listener/bind/router tests；已实现                      |
| Persisted-field allowlist          | Field catalog + persistence manifest + privacy surface | Generator/privacy/crate-boundary checks；已实现         |
| Forbidden content 不持久化         | Typed extraction deny boundary                         | L1 marker scan、SQLite/log/API tests；已实现            |
| Memory-only raw decoding           | Raw bytes/map 只在 telemetry request 内                | Decoder limits、port boundary、dependency guard；已实现 |
| Protected local Dashboard API      | Session/bootstrap/Host/Origin/CSRF                     | Dashboard security tests；已实现                        |
| Current-user local files           | `storage/paths.rs` 与 owner-only registrations         | Synthetic path/platform backend tests；已实现           |
| Safe structured diagnostics        | Telemetry structured log 与 closed codes               | Omission/schema tests；已实现                           |
| Privacy-safe notifications         | Typed params/fixed templates                           | Privacy gate、renderer tests、M1                        |
| Privacy-preserving config guidance | 中英文 README/install/troubleshooting snippets         | Document relation/checks；已实现                        |
| Legacy import 不扩大采集           | Read-only allowlisted historical projection            | L1、G1、import tests；已实现                            |

## `reasoning-alerts`

| Requirement                              | 当前实现                                     | 证据与状态                                          |
| ---------------------------------------- | -------------------------------------------- | --------------------------------------------------- |
| Reasoning quantization pattern detection | `core/rule.rs` closed formula                | 516/higher/neighbor/missing property matrix；已实现 |
| 每次 match 保留 evidence                 | Transactional alert persistence              | Policy/provenance/replay tests；已实现              |
| Configurable notification threshold      | Rule settings + `SettingsRuntime` projection | Defaults/all/CAS tests；已实现                      |
| Heuristic non-assertive messaging        | Fixed bilingual suspected-pattern copy       | Localization/renderer/Playwright；M1                |
| Versioned idempotent evaluation          | Registry fingerprint + fenced work           | Replay/drift/restart tests；已实现                  |
| Historical-event isolation               | Immutable call origin 与 DB/worker guards    | Import/upgrade/restart tests；已实现                |
| Coalescing without evidence loss         | Durable 30-second windows                    | Boundary/concurrency/restart/rollback tests；已实现 |
| Rule availability observable             | Closed `RuleErrorCode` capability            | Quarantine/supervisor/recovery tests；已实现        |

## `resident-monitor-host`

| Requirement                  | 当前实现                                                    | 证据与状态                                                             |
| ---------------------------- | ----------------------------------------------------------- | ---------------------------------------------------------------------- |
| Resident user-session app    | Desktop owns one `ResidentEngine`，无 native window/service | Host/desktop/product tests；L2                                         |
| Manual/background startup    | `codex-monitor-host::startup` + coordinator                 | Fake-port/startup/lifecycle tests；L2                                  |
| Explicit login autostart     | Desktop platform backends + settings runtime                | Exact Windows/Linux/macOS registration tests；L2，其他平台 H1          |
| Single instance              | Tauri plugin boundary + closed forwarded command            | Untrusted/forwarded tests；L2                                          |
| Cross-platform tray controls | Host view model + desktop native presentation               | Status/render/menu/capability tests；native matrix defined，H1 applies |
| Ordered shutdown             | Engine stages + host lifecycle/tray cleanup                 | Deadline/failure/idempotence/product tests；L2                         |
| Reusable UI host boundary    | Redacted URL；browser/future window share HTTP origin       | Adapter tests、ADR 0004；已实现                                        |

## `telemetry-ingestion`

| Requirement                  | 当前实现                                            | 证据与状态                                         |
| ---------------------------- | --------------------------------------------------- | -------------------------------------------------- |
| Loopback OTLP logs endpoint  | `telemetry/receiver.rs` 固定 `127.0.0.1:4318`       | Bind/minimal-router/retry tests；L1                |
| OTLP encoding compatibility  | `telemetry/decoder.rs`                              | JSON/Protobuf/gzip matrix；L1                      |
| Safe limits/errors           | Bounded decoder/request deadline/closed status      | Expansion/body/deadline/malformed tests；已实现    |
| Allowlisted Codex extraction | `telemetry/otlp.rs` + core observation              | Unknown/forbidden/metadata/property tests；L1      |
| Commit-before-ack            | Telemetry `ObservationSink` + engine storage bridge | Processor cancellation/retry/product tests；已实现 |
| Source-level dedup           | Stable source identity + unique storage constraints | Concurrent retransmission/restart tests；已实现    |
| Extensible source contract   | Core source types + telemetry `TelemetrySource`     | Version/port/capability/how-to evidence；已实现    |
| Receiver failure diagnosable | Receiver monitor、Dashboard DTO、host tray state    | Retry/security/frontend/fake-port tests；R1        |

## Accepted v1 boundary 与 deferred evidence

1. Windows fresh-profile 与 copied synthetic legacy import 由 L2 覆盖，作为 portable-only v1 release boundary 接受。
2. M1 可见 Windows notification 于 2026-07-17 人工确认。
3. Installer-upgrade、macOS x64、macOS arm64 与 Linux x64 的历史 v1 native execution 仍标记 H1 deferred，不追溯声称完成。

Workflow definition、release-contract test 与 installed-runner source 只能证明收集 evidence 的准备状态，不能替代 hosted/manual execution evidence。
