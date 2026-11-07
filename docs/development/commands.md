# 标准开发命令

仓库固定 Rust 1.97.0 与 pnpm 11.13.0，Node.js 需要 24 或更高版本。首次使用或 lockfile 变化后运行 `pnpm install --frozen-lockfile`。

## 日常反馈节奏

从仓库根目录按自然检查点工作：

```text
cargo build -p <受影响 crate>       # 日常编译与链接反馈
cargo check -p <受影响 crate>       # 日常主要编译反馈
cargo check -p <crate> --tests       # 修改 cfg(test) 或测试工具
cargo check --workspace              # core、公共 trait/DTO 或 workspace 依赖收敛后
cargo test -p <受影响 crate>         # 行为稳定后的局部测试
pnpm verify:changed -- --changed <path> # 显示按路径选择的检查
```

不要在每次源码编辑后运行全 workspace Clippy、test、build 或 `pnpm verify`。完整门禁只对最终稳定代码运行。

## Rust 调试 profile

日常 `dev` 与继承它的内置 `test` profile 使用有限调试信息，保留函数、文件和行号等常用诊断能力，同时控制 PDB、对象文件与 Rust 库的体积。只有常规日志、诊断和局部调试仍无法定位问题，且需要通过调试器查看完整类型、参数和局部变量时，才改用独立的 `full-debug` profile：

```text
cargo build -p codex-monitor-desktop --profile full-debug
cargo test -p <受影响 crate> --profile full-debug
cargo clean --profile full-debug
```

`full-debug` 产物位于 `target/full-debug/`，与日常 `target/debug/` 隔离。只在疑难问题调查期间保留；调查结束后清理，避免长期保存完整调试产物。

## 调试 profile 基准记录

遇到需要评估编译、链接或调试信息代价的改动时，在同一提交、同一 Rust 工具链、相同硬件和相同依赖缓存状态下记录下列命令的耗时、Cargo 输出中的编译/链接阶段，以及调试器能否查看所需的类型、参数和局部变量：

```powershell
Measure-Command { cargo build -p codex-monitor-desktop }
Measure-Command { cargo test -p <受影响 crate> }
Measure-Command { cargo build -p codex-monitor-desktop --profile full-debug }
Measure-Command { cargo test -p <受影响 crate> --profile full-debug }
```

记录时注明冷缓存或热缓存、目标 crate、提交、命令、总耗时和调试器验证结果；编译与链接耗时以 Cargo 的阶段输出为准。`full-debug` 诊断完成后运行 `cargo clean --profile full-debug`。本轮不采用 `line-tables-only`；任何后续调试信息策略调整都必须先给出可复现收益，并证明函数、文件、行号、类型、参数和局部变量等诊断能力没有下降。

### 2026-07-21 基准记录

以下记录在 Windows 工作区、Rust 1.97.0 上执行，默认 profile 复用已有 `target/debug` 热缓存，`full-debug` 在开始前已清理其隔离目录。时间来自 `Measure-Command` 与 Cargo 的 `Finished` 输出；两者不同表示 PowerShell 启动和测试运行开销。

| 命令                                                        | 缓存状态                                     | 编译/链接耗时            | 测试结果与调试证据                                                           |
| ----------------------------------------------------------- | -------------------------------------------- | ------------------------ | ---------------------------------------------------------------------------- |
| `cargo build -p codex-monitor-desktop`                      | `target/debug` 热缓存                        | 17.30 秒；Cargo 16.96 秒 | desktop 编译与链接成功。                                                     |
| `cargo test -p codex-monitor-core`                          | `target/debug` 热缓存                        | Cargo 0.77 秒            | 52/52 unit tests 通过，测试正文 0.75 秒。                                    |
| `cargo build -p codex-monitor-desktop --profile full-debug` | 清理后的 `target/full-debug` 首次构建        | Cargo 2 分 11 秒         | desktop 编译与链接成功；生成 `target/full-debug/codex_monitor_desktop.pdb`。 |
| `cargo test -p codex-monitor-core --profile full-debug`     | desktop full-debug 构建后；core 测试首次编译 | 21.42 秒；Cargo 19.46 秒 | 52/52 unit tests 通过，测试正文 0.26 秒。                                    |

完整调试 profile 的首次 desktop 构建比热缓存默认构建多约 114 秒，因此它仅用于疑难诊断。上述 PDB 证明完整调试信息产物已生成；本次没有待排查的运行时问题，未执行交互式断点、局部变量或调用栈会话，不能把它当作调试器体验的替代证据。检查后已运行 `cargo clean --profile full-debug`，删除 5,472 个文件、约 4.7 GiB，隔离目录不再保留。

## 分类命令

```text
pnpm format                 # 写入 Rust 与仓库格式
pnpm format:check           # 只检查格式
pnpm lint:rust              # workspace Clippy，warnings 视为错误
pnpm lint:web               # frontend static analysis
pnpm lint                   # 依次运行两类 lint
pnpm test:rust              # 一次 cargo test --workspace
pnpm test:tooling           # 文档、生成器、选择器、CI/release 合同测试
pnpm test:web:unit          # Vitest
pnpm test:web:e2e           # 完整四项目 production Playwright matrix
pnpm test:product-integration # 真实 SQLite/listeners/workers/Dashboard/host 组合
pnpm test:release           # 聚焦 native release contract
pnpm build:app              # 先构建 Dashboard，再构建 desktop 生产二进制
pnpm generate               # 所有已提交生成产物
pnpm docs:check             # 登记、导航、语言、链接、生成漂移与 impact
```

Playwright 的开发期入口为 `test:e2e:smoke`、`test:e2e:responsive` 与 `test:e2e:locales`；公共路由、session、CSRF、安全或共享布局修改使用 `test:e2e:production`。

## 完整门禁

Rust 功能完成时按以下顺序验证最终树：

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
pnpm --dir web build
cargo build -p codex-monitor-desktop
```

`pnpm verify:rust` 提供同一 Rust 功能完成门禁。`pnpm verify` 读取 `docs/_meta/verification-map.yaml` 的 `full_checks`，依次运行快速检查、lint、单元/工具测试、产品集成与构建、完整浏览器矩阵，并输出每项与每阶段耗时。Clippy 与 Rust tests 串行运行，避免争用同一 Cargo target。

## 隐私与安全

```text
pnpm check:privacy
pnpm check:repository-fixtures
pnpm check:repository-fixtures:worktree
pnpm check:no-legacy-runtime
pnpm check:licenses
pnpm audit:rust
pnpm audit:security
```

Fixture gate 默认读取 Git index 中的文件名与 blob；`:worktree` 变体用于提交前和 `pnpm verify`，检查 tracked 与未跟踪且未忽略的当前候选文件，并排除已删除路径。no-legacy gate 同样检查当前候选文件。不得使用个人 Codex database 或 log 运行测试，只能使用仓库 synthetic fixture。

完整 online audit 需要 `cargo-deny` 0.20.2、`cargo-audit` 0.22.2 与 Gitleaks 8.30.1。CI 只在依赖/安全配置 PR、`main`、定时或显式完整执行时安装并运行它们；secret scan 覆盖每个 PR。

Release workflow 的 native matrix 为 Windows x64、macOS x64、macOS arm64 与 Linux x64。安装产物、应用身份、通知服务和平台权限的执行证据来自关联 workflow run；本机 `pnpm verify` 不能代替这些 native effects。
