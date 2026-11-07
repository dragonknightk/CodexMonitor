# 测试策略

## Crate 局部测试

- `codex-monitor-core`：领域 validation、provenance、identity、rule/action policy 与 serialization/property tests。
- `codex-monitor-storage`：临时 SQLite、migration ledger、legacy import、query projection、retention、Outbox、change feed 与 writer crash/replay。
- `codex-monitor-telemetry`：Protobuf/JSON、gzip、request limits、malformed input、typed extraction、receiver retry/shutdown、session index 与 source health。
- `codex-monitor-dashboard`：assets、routing、request limits、Host/Origin/CSRF/cookie、session/bootstrap、cursor、REST response、SSE control 和消费方端口 fake。
- `codex-monitor-engine`：rule/action workers、settings coordinator、bridge、startup cancellation、supervision 与 shutdown budgets。
- `codex-monitor-host`：使用 fake `ResidentHostPort` 测试 manual/background/forwarded startup、port conflict presentation、status update 与幂等退出；不编译 Tauri。
- `codex-monitor-desktop`：原生 adapter、tray render、autostart canonical registration、notification privacy 与 Tauri lifecycle integration。

日常先运行受影响 crate 的 `cargo check`。行为稳定后运行局部 `cargo test -p ...`。公共 core/port/DTO 收敛后运行一次 `cargo check --workspace`。最终 Rust 集合只运行一次 `cargo test --workspace`，不追加重复的 `--tests` 聚合。

## 调试 profile 使用约定

普通 `cargo build`、`cargo check` 和 `cargo test` 使用内置 `dev`/`test` profile，日常测试不需要指定 profile。只有常规日志、诊断和局部调试仍无法定位问题时，才使用 `cargo test -p <受影响 crate> --profile full-debug`。该 profile 的产物固定在 `target/full-debug/`；定位后运行 `cargo clean --profile full-debug`，避免完整调试信息污染日常构建输出。`release` profile 不因上述诊断约定改变。

## 2026-07-21 完整验证基准

在最终工作树上执行 `pnpm verify`，验证器选择 18 项检查且全部通过，总耗时为 94.01 秒。该次运行复用前序检查生成的缓存；相对 176.80 秒缓存态基线缩短 82.79 秒（约 46.8%），但仍比 90 秒目标高 4.01 秒。

| 阶段           |     耗时 | 关键组成                                                                              |
| -------------- | -------: | ------------------------------------------------------------------------------------- |
| 快速检查       | 17.86 秒 | 8 项，包括格式、边界与文档检查。                                                      |
| 静态分析       | 14.61 秒 | Rust Clippy 3.54 秒，web lint 11.07 秒。                                              |
| 单元与工具测试 | 38.57 秒 | Rust tests 13.67 秒、工具测试 12.95 秒、web unit 10.70 秒、release contract 1.26 秒。 |
| 产品集成与构建 | 11.38 秒 | product integration 1.49 秒，Dashboard 与 desktop build 9.89 秒。                     |
| 浏览器矩阵     | 11.58 秒 | 四项目 Playwright matrix，12/12 通过。                                                |

剩余瓶颈是单元与工具测试阶段，主要由 Rust、工具和 web unit 三个互不重复的测试集合构成；本轮没有删除、合并或弱化其中任何测试。90 秒目标尚差 4.01 秒，后续优化应先对这个阶段做可复现的细分测量，再调整执行方式或缓存策略。

同次运行代表性的 changed-scope dry-run：

```text
pnpm verify:changed -- --changed apps/desktop/src-tauri/src/platform_registration.rs --changed .github/workflows/release.yml --dry-run
```

该用例选择 crate 边界、desktop check、文档、Rust、工具、产品集成和应用构建；`test-rust` 明确覆盖 `desktop-test`，没有重复执行被覆盖的检查。

## 产品集成

`cargo test -p codex-monitor-engine --test product_integration` 是明确的完整产品组合目标。它使用真实临时 SQLite、两个 loopback listeners、workers、Dashboard、host coordinator 与有序关闭，不复制各 crate 的局部边界矩阵。

Host 命令与视图状态在 `codex-monitor-host` 通过 fake port 确定性验证；产品集成只保留一条真实 adapter 路径。这样可以缩小局部编译闭包，同时保留启动、readiness、port conflict、Dashboard privacy 与资源释放证据。

## Frontend 与浏览器

Dashboard 使用 component tests 验证 state/accessibility，使用生成 OpenAPI types 约束 API，并使用 Playwright 的四个项目验证 desktop/mobile 与 `en-US`/`zh-CN`。

- `test:e2e:smoke`：单个 desktop `en-US` 快速入口。
- `test:e2e:responsive`：desktop/mobile，固定一个语言。
- `test:e2e:locales`：中英文 desktop。
- `test:e2e:production`：desktop/mobile × `en-US`/`zh-CN` 完整 production build matrix。

共享路由、session、CSRF、安全、SSE synchronization 或共享布局必须升级到完整四项目；最终交付也运行完整矩阵。

## 原生安装产物

Release workflow 在 Windows x64、macOS x64、macOS arm64 和 Linux x64 运行真实打包与 installed-artifact smoke，覆盖 single-instance transport、browser、tray、login autostart、notification backend 与 `--import-legacy <absolute-sqlite-path>`。

Bootstrap smoke 必须跟随 `303` redirect、携带新 session cookie 获取打包 HTML，提取全部 `/assets/` URL，并验证每个 JS/CSS 的 MIME、cache 与 `nosniff` header。只检查 bootstrap 或单个 API operation 不足以证明安装后的浏览器可加载 Dashboard。

## 隐私与仓库边界

Privacy assertions 检查 SQLite bytes/logical rows、structured logs、API responses 与 notification parameters，确保 synthetic forbidden markers 不出现。Load tests 对 queue、latency 和 memory 设置明确预算。

Fixture gate 读取 staged Git blobs，拒绝 tracked local database、sidecar、JSONL/log capture、非模板 environment file 与 key material。No-legacy gate 检查 tracked 加 untracked non-ignored candidates，拒绝 Python source/toolchain 与已删除 runtime entry point。

Privacy-surface gate 把 migration 构造出的完整 schema 与 `SystemNotificationParams` 对照 reviewed manifest。任何 column 或 notification parameter 变化都会失败，直到 classification、rationale 和隐私文档完成审查。
