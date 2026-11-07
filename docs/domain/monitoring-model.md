# 监控领域模型

领域模型把用户可见的组织结构、单次模型工作和来源证据分开管理。

- `Conversation` 表示长期存在的 Codex conversation 或 thread 容器。
- `Turn` 表示一次用户可见的交互。来源无法证明关系时，所属关系保持未知。
- `ModelCall` 表示一次已完成的模型请求，是 token 值和实时规则求值的原子单位。
- `ModelCallObservation` 表示一个来源为某次调用提供的一组允许字段证据。

内部 `ConversationId`、`TurnId`、`ModelCallId` 和 `ModelCallObservationId` 使用 UUIDv7。来源提供的 conversation、thread 和 turn 标识符使用独立的有界类型，adapter 无法把外部关联键误当成内部关系。一次调用只有在 `ConversationId` 已知时才能携带 `TurnId`；关系尚未证明时，来源 turn 标识符只作为证据保存。

## Observation 与 provenance

跨 adapter 的 observation 合同定义在 `codex-monitor-core`，只含字段目录允许的闭合类型。原始 protobuf、请求字节、任意 attribute map 和 generic JSON 留在 `codex-monitor-telemetry` 内部，不进入领域或持久化边界。

每个 canonical 字段保留来源身份、observation 时间和 `direct` 或 `inferred` 质量。直接事件值优先于推断值；低质量证据不会覆盖已知的高质量值。互补来源可以填写不同字段，每条 observation 仍可独立审计。

比较顺序为证据质量、已配置的来源优先级、稳定来源身份。接收时间只用于审计，不参与优先级，因为重放时到达顺序可能变化。完全相同 provenance 下出现矛盾时，以类型化值的稳定排序作为最终 tie-break，保证结果与重放顺序无关。事件时间遵循同一字段级 provenance 政策；实体首次和末次观察时间属于本机 UTC 生命周期元数据。

来源优先使用稳定事件标识符。缺失稳定标识时，来源 adapter 仅从允许的来源、时间、conversation、model 和 token 字段生成带版本的确定性 fingerprint。身份算法版本属于持久身份的一部分，升级不能静默重解释历史唯一键。

每个 canonical model call 同时保留内部 UUID 与最初用于去重的版本化 `SourceEventIdentity`。后续来源 observation 可以关联到同一次调用，但不能替换其稳定去重身份。

所有时间戳以 UTC 跨越领域边界。Token 数量必须是字段目录上限内的非负整数。未知的可选值继续保持未知；系统不会虚构 turn、reasoning effort、client 或 token 值。

## 规则与告警

实时完成调用与唯一的 `model_call.completed` 工作项在同一 writer transaction 中提交。规则 worker 使用带 fencing generation 的 claim 处理 pending 或 stale 工作，在独立读连接加载 canonical call，并在写入结果前重新验证 snapshot。

内置规则版本登记不可变语义 fingerprint。公式、字段、操作符或参数发生语义变化时必须增加版本；同一版本发生漂移会使启动失败。新版本继承稳定 rule ID 的启用状态和通知阈值，同时保留独立的历史求值记录。

每次匹配在规则完成 transaction 中创建独立 alert，记录 model call、规则版本、reasoning token 值、pattern number、事件时间、observation 时间、来源和证据质量。系统通知关闭或匹配超过阈值时，alert 仍保留。

规则健康只使用闭合 `RuleErrorCode`。当前允许 `persistence_commit_failed` 和 `internal_queue_closed`；payload、标识符和自由文本错误不能进入 capability 文档。格式错误的内部 canonical snapshot 会被终结为已隔离工作，worker 随后继续处理下一项。

## 动作领域

动作使用闭合类型化 registry。v1 只登记 `system_notification`，其参数 schema 接受可选的有界 model 名称、token 数量和正整数 pattern number。`ActionRequest` 把这些参数绑定到 alert 与 model call，记录参数 schema 版本，并从 alert ID、动作类型与 schema 版本生成稳定的版本化幂等 digest。随机 action ID 只标识审计行。

持久生命周期为 `pending`、`started`，随后进入且只进入 `completed`、`failed`、`suppressed` 或 `unavailable`。开始的 attempt 记录授权它的 lease generation。终态失败使用闭合安全错误码，effect 前抑制使用闭合 reason enum。两者都不能携带来源文本、prompt、路径、URL、命令或任意序列化参数。

Host executor 必须声明与 registry 完全一致的 capability 和已存权限。未知动作、schema 不匹配和非法类型参数会进入 `unavailable`，不存在 fallback executor。进程重启后仍处于 `started` 的 attempt 会变为 `indeterminate`，其 action 变为 `failed`，两者都不会返回可执行状态。

执行时还会收到进程内 `ActionExecutionContext`。Action worker 在调用 host 前读取当前应用语言，并只在持久 coalescing leader 已有 follower 时设置 grouped 标记。该 context 不属于动作证据且不持久化，使 host 能从固定模板选择本地化文案，无需转发设置文档或任意文本。

只有 canonical `ConversationId` 已知时才使用通知合并。第一个 eligible action 打开固定 30 秒半开区间；区间内后续 alert 各自保留 terminal `coalesced` action audit 并链接 leader。区间不会被 follower 延长，恰好位于结束时间的请求打开新窗口。未知 conversation 关系保持独立，来源 ID、thread ID 和显示名称都不能作为合并键。

## 隐私权威

字段隐私权威是 `docs/_meta/telemetry-fields.yaml`。生成参考文档与 `crates/codex-monitor-core/telemetry-persistence-fields.yaml` 必须完全一致，CI 才接受字段进入持久化边界。

Conversation 显示名称只在经过身份验证的查询期间从本机 session index 解析，永不写入 SQLite、日志、alert 或通知。Raw payload、用户内容、reasoning 内容、tool data、命令、路径、账户标识符和认证材料永久禁止进入监控领域。
