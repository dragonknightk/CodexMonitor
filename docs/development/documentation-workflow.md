# 文档工作流

## 登记与导航

`docs/_meta/documentation-map.yaml` 是受管 Markdown 与实现影响的机器可读目录。Version 2 的每个 document entry 记录 explicit path 或 controlled glob、category、document language、authority language、translation relation、generator、historical immutable state 与 navigation kind。

`pnpm docs:check` 验证：

- 所有受管 Markdown 恰好匹配一个登记项；
- 仓库中不存在任何 `AGENTS.md` 指令文件；
- `docs/**` 中的普通文档可从 `docs/index.md` 沿本地链接到达；
- 中文 authority 含中文叙述，英文用户翻译指向中文 authority 并保持 heading structure；
- 已接受英文 ADR 原件不被修改；
- 生成输出与已登记 authority/generator 一致；
- 本地 Markdown path 与 anchor 都存在。

根 README（`README.md` 与 `README.en.md`）、privacy 与 PR template 因平台发现要求保留固定位置，但仍登记类别和语言关系。普通说明放在 `docs/**`。

## Documentation impact

同一 map 的 `mappings` 把 implementation globs 映射到 required/optional documents、generators 与 checks。PR 使用 `node scripts/check-docs.mjs --base <git-ref>` 读取实现 diff。

Mapped behavior 变化时，每个匹配 mapping 至少更新一个 required target。确实没有文档影响时传入 `--docs-impact none --reason "<具体解释>"`；空理由、通用理由或不足 20 字符的理由会失败。

## 生成 authority

Telemetry 字段 allowlist 位于 `docs/_meta/telemetry-fields.yaml`，持久化精确清单位于 `crates/codex-monitor-core/telemetry-persistence-fields.yaml`。修改后运行 `pnpm generate:references`，生成器会拒绝未登记的 persisted field 和 deny-list term。

Telemetry source authority 位于 `docs/_meta/telemetry-sources.yaml`。Database schema authority 位于 `crates/codex-monitor-storage/migrations`。Schema generator 按顺序应用 migrations，识别 SQLite rebuild 使用的严格 `DROP`/`RENAME` 形式；未知 schema-changing form 失败关闭。

```text
node scripts/generate-references.mjs
node scripts/generate-telemetry-fields.mjs --persistence-manifest crates/codex-monitor-core/telemetry-persistence-fields.yaml
node scripts/generate-database-schema.mjs
```

HTTP API authority 是 `api/openapi.yaml`，生成输出是 `web/src/api/schema.d.ts`。修改 authority 后运行 `pnpm --dir web generate:api` 与 `pnpm check:api`。

生成 Markdown 禁止直接编辑；先修改 authority 或中文 generator template，再运行登记 command。

## Privacy 与 fixture

`docs/_meta/privacy-surfaces.yaml` 必须按确切顺序列出 migration-built SQLite table/column 和所有 dynamic `SystemNotificationParams` field，并提供 classification 与 rationale。`pnpm check:privacy` 同时检查 telemetry deny terms、schema、notification 参数和中英文 privacy 声明。

`node scripts/check-repository-fixtures.mjs` 默认以 Git index 为输入边界，通过 object ID 读取 staged blob，适合提交和安全审计。`pnpm check:repository-fixtures:worktree` 检查当前工作区候选文件，包含未跟踪且未忽略的文件，并排除已经删除的 tracked path；`pnpm verify` 使用该模式。两种模式都拒绝 database、sidecar、JSONL/log capture、non-template environment file 和 key material。`fixtures/otlp` 只允许 frozen contract 登记的 synthetic file；失败信息只包含 rule name 与 repository-relative path。

`pnpm check:no-legacy-runtime` 检查当前 candidate tree，包括仍存在的 tracked files 与 untracked non-ignored files。Deleted tracked path 视为不存在。它拒绝 Python source/toolchain、旧 prototype path 和恢复 removed runtime 的 command/dispatch。

## Changed-scope verification

`docs/_meta/verification-map.yaml` 登记 crate、migration、field catalog、OpenAPI、web、docs、toolchain、build 与 security paths。`scripts/verify-changed.mjs` 从 Git diff 选择 checks、合并重复项、打印每个理由和升级条件，并记录 check/stage/total duration。未知路径失败，要求先补登记。

保持导航简短。某一节成为独立任务时新建 focused document，从[文档索引](../index.md)链接，并为影响它的实现范围增加 mapping。
