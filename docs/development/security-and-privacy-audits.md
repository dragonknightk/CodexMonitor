# 安全、供应链与隐私审计

本文记录 repository security gate、精确接受的临时 exceptions，以及 persistence/system-notification field review。新 advisory ID、license expression、package name 或 privacy surface 不会自动继承现有 exception。

## 命令与 CI 边界

| 命令                                      | 覆盖                                                                           | 本机前置条件                                    |
| ----------------------------------------- | ------------------------------------------------------------------------------ | ----------------------------------------------- |
| `pnpm check:privacy`                      | 精确 SQLite 与 system-notification surface review                              | Workspace dependencies；offline                 |
| `pnpm check:repository-fixtures`          | Staged local-data types 与 synthetic OTLP fixture                              | Candidate 已 staged；offline                    |
| `pnpm check:repository-fixtures:worktree` | 当前工作区 local-data types 与 synthetic OTLP fixture                          | 无；offline                                     |
| `pnpm check:no-legacy-runtime`            | 已删除 Python artifacts、entry points 与 executable dispatch                   | 无；offline                                     |
| `pnpm check:licenses`                     | Frontend license allowlist 与 named exceptions                                 | Frozen pnpm install                             |
| `pnpm audit:rust`                         | `cargo deny` advisories/licenses/bans/sources 与 warning-denying `cargo audit` | `cargo-deny` 0.20.2、`cargo-audit` 0.22.2、网络 |
| `pnpm audit:security`                     | Privacy/license、package audit、Rust audit 与 Gitleaks                         | Gitleaks 8.30.1 及上述工具                      |

`pnpm verify` 包含 offline privacy/cutover checks，不自行安装 audit 工具。CI 的完整 security job 只在 dependency/security configuration PR、`main`、weekly schedule 或显式完整执行时运行；它校验 Gitleaks Linux archive SHA-256。独立 `Secret scan` 保持覆盖每个 PR 与 Git history。

Rust advisory policy 由两个独立 engine 执行。`deny.toml` 检查 transitive unmaintained/unsound advisory、yanked crate 和 source policy；`cargo audit -D warnings` 让 vulnerability、informational 与 yanked finding fatal。17 个重复 `--ignore` 只接受下列已审查 ID。

## 2026-07-15 review snapshot

以下结果在 wiring change 前实际观察到；未列为 passed 的检查不声称已通过。

| 检查                                            | 观察结果                                                                                               |
| ----------------------------------------------- | ------------------------------------------------------------------------------------------------------ |
| `node scripts/check-frontend-licenses.mjs`      | Passed：268 package records、10 allowed expressions、3 reviewed exception groups                       |
| `node scripts/check-privacy-surfaces.mjs`       | Passed：27 SQLite tables、214 columns、3 notification parameters                                       |
| `cargo deny check licenses bans sources` 0.20.2 | Passed；duplicate versions 只产生 warnings                                                             |
| `cargo audit 0.22.2 --json`                     | 523 dependencies、0 vulnerabilities、16 unmaintained 与 1 unsound advisory；database 更新于 2026-07-13 |
| `pnpm audit --audit-level high`                 | Passed，0 vulnerabilities                                                                              |
| `gitleaks dir` 8.30.1（加入本机环境排除前）     | 唯一 finding 位于 untracked `.venv` installed dependency，不属于 repository input                      |
| Policy wiring 后 `pnpm audit:security`          | Passed，覆盖 privacy/license、pnpm audit、两套 Rust advisory engine 与 Gitleaks                        |
| Gitleaks 8.30.1 archive checksum                | Pinned SHA-256 与 release manifest 相符                                                                |

Clean-environment authoritative execution 来自 CI；本文的 snapshot 不替代新的 workflow run。

## Frontend license exceptions

Checker 以精确 license expression 与 package name 为键，version update 仍需重新检查 role 与 terms。

| Expression                                                | Packages                 | 审查与移除条件                                                                                                                |
| --------------------------------------------------------- | ------------------------ | ----------------------------------------------------------------------------------------------------------------------------- |
| [BlueOak-1.0.0](https://blueoakcouncil.org/license/1.0.0) | `lru-cache`、`minimatch` | Repository tooling/test dependency 的 permissive grant 已审查；成为直接生产依赖或 license 实质变化时重审，离开 graph 时移除。 |
| [CC-BY-4.0](https://creativecommons.org/licenses/by/4.0/) | `caniuse-lite`           | Build tooling 的 browser compatibility data 具有 attribution 条件；数据进入 distributed runtime artifact 时重审。             |
| [Python-2.0](https://spdx.org/licenses/Python-2.0.html)   | `argparse`               | Permissive transitive tooling dependency；变为 direct 或 shipped runtime 时重审。                                             |

同一 expression 下其他 package 或其他 expression 都会使 `pnpm check:licenses` 失败。

## Rust advisory exceptions

17 项全部来自 transitive Tauri 2 dependencies，没有 workspace direct dependency。16 项 unmaintained advisory 没有当前 upstream graph 可用的安全升级，仍属于 release debt，每次 Tauri/lockfile update 都要审查。

### GTK3 bindings：10 项 unmaintained

Linux path 从 `codex-monitor-desktop` 经 Tauri `tao`、`wry`、`webkit2gtk`、tray/runtime 进入 GTK 3：

- [RUSTSEC-2024-0411](https://rustsec.org/advisories/RUSTSEC-2024-0411)：`gdkwayland-sys`
- [RUSTSEC-2024-0412](https://rustsec.org/advisories/RUSTSEC-2024-0412)：`gdk`
- [RUSTSEC-2024-0413](https://rustsec.org/advisories/RUSTSEC-2024-0413)：`atk`
- [RUSTSEC-2024-0414](https://rustsec.org/advisories/RUSTSEC-2024-0414)：`gdkx11-sys`
- [RUSTSEC-2024-0415](https://rustsec.org/advisories/RUSTSEC-2024-0415)：`gtk`
- [RUSTSEC-2024-0416](https://rustsec.org/advisories/RUSTSEC-2024-0416)：`atk-sys`
- [RUSTSEC-2024-0417](https://rustsec.org/advisories/RUSTSEC-2024-0417)：`gdkx11`
- [RUSTSEC-2024-0418](https://rustsec.org/advisories/RUSTSEC-2024-0418)：`gdk-sys`
- [RUSTSEC-2024-0419](https://rustsec.org/advisories/RUSTSEC-2024-0419)：`gtk3-macros`
- [RUSTSEC-2024-0420](https://rustsec.org/advisories/RUSTSEC-2024-0420)：`gtk-sys`

Tauri Linux stack 不再解析 archived GTK3 bindings 时应一并移除。任一 ID 被重新分类、同版本出现 non-informational advisory 或 crate 变成 direct dependency 时立即重审。

### `proc-macro-error`：1 项 unmaintained

[RUSTSEC-2024-0370](https://rustsec.org/advisories/RUSTSEC-2024-0370) 覆盖 `proc-macro-error 1.0.4`，通过 `glib-macros`/`gtk3-macros` 进入 Linux GTK3 graph。GTK macro stack 移除该 crate 后删除 exception；出现 vulnerability 或 direct dependency 时立即重审。

### rust-unic：5 项 unmaintained

这些 crates 通过 `tauri-utils -> urlpattern -> unic-ucd-ident` 进入 runtime、code generation 与 build dependencies：

- [RUSTSEC-2025-0075](https://rustsec.org/advisories/RUSTSEC-2025-0075)：`unic-char-range`
- [RUSTSEC-2025-0080](https://rustsec.org/advisories/RUSTSEC-2025-0080)：`unic-common`
- [RUSTSEC-2025-0081](https://rustsec.org/advisories/RUSTSEC-2025-0081)：`unic-char-property`
- [RUSTSEC-2025-0098](https://rustsec.org/advisories/RUSTSEC-2025-0098)：`unic-ucd-version`
- [RUSTSEC-2025-0100](https://rustsec.org/advisories/RUSTSEC-2025-0100)：`unic-ucd-ident`

`tauri-utils`/`urlpattern` 迁移到 maintained Unicode dependencies 后全部移除。New advisory、new path 或 direct use 都触发重审。

### `glib`：1 项较高风险 unsound advisory

[RUSTSEC-2024-0429](https://rustsec.org/advisories/RUSTSEC-2024-0429) 影响 `glib 0.18.5`，在 `glib >=0.20.0` 修复。受影响的 `glib::VariantStrIter::{next, next_back, nth, nth_back, last}` 可能造成 undefined behavior 或 null-pointer crash。当前 Tauri GTK3 graph 约束 `glib 0.18`，不能单独更新 lockfile。

CodexMonitor 没有直接 `glib`/`VariantStrIter` call site；Windows/macOS 不解析该 Linux runtime path。缺少直接调用无法证明所有 upstream path 都不可达，因此这是较高风险临时 exception。Tauri Linux stack 采用 `glib >=0.20.0` 或移除受影响 GTK stack 后立即删除；upstream 开始使用相关 API、dependency 变 direct 或 advisory surface 扩大时立即重审。

## Cargo source 与 freshness policy

`multiple-versions = "warn"` 让 duplicate crate 可见但不阻塞当前 Tauri graph。Warning 只表示 inventory signal，不是 advisory acceptance。

`wildcards = "allow"` 受 cargo-deny 0.20.2 限制：该版本把 workspace local path dependencies 报为 wildcard。Manifest/lockfile review 必须继续人工检查 registry wildcard。Unknown registry 与 unknown Git source 仍拒绝；只允许 crates.io，不允许 Git source。

`pnpm-workspace.yaml` 的 `minimumReleaseAge: 4320` 将新 package 延迟 3 天。`minimumReleaseAgeExclude` 只列精确 reviewed version；`postcss@8.5.19` 是 policy 引入时尚未满 3 天但已锁定审计的例外，后续 resolution 不再需要时移除。

## Secret scan 范围

`.venv` 是本机 installed environment，不属于 repository input，因此排除。`target`、`node_modules` 和 Dashboard reports 等 dependency/build output 同样排除。Source 与 synthetic fixture directories 不排除；其中 finding 会同时使 working-tree aggregate scan 与 Git-history scan 失败。

## Persistence 与 notification field review

`docs/_meta/privacy-surfaces.yaml` 对照 `docs/_meta/telemetry-fields.yaml`、`crates/codex-monitor-core/telemetry-persistence-fields.yaml`、`PRIVACY.md` 与 `PRIVACY.zh-CN.md` 审查。2026-07-15 snapshot 包含 27 SQLite tables、全部 214 columns，以及精确 3 个 dynamic notification parameters：`model`、`reasoning_output_tokens`、`pattern_n`。

`pnpm check:privacy` 从 embedded migrations 构造最终 schema，按精确顺序比较 table/column，拒绝新增、删除、rename、unknown classification、missing rationale 与 telemetry deny term。它解析 `SystemNotificationParams` 并要求 exact field list，再验证 manifest 只含 `en-US`/`zh-CN` forbidden term sets 且每项出现在对应 privacy document。

持久字段或 notification parameter 变化必须在同一 change 更新 manifest、解释 purpose/classification，并按需更新 telemetry catalog、persistence manifest 或 privacy document，再通过 privacy-specific tests 与 aggregate gate。
