# ADR 0006：类型化隐私与 action registries

[历史英文原件](0006-typed-privacy-action-registries.md)是不可变权威记录；本文件是中文伴随译本。

## 状态

2026-07-15 接受。

## 背景

OTLP 输入可以包含任意 map、内容和标识符，durable actions 可以产生操作系统效果。持久化 generic decoded data 或按字符串选择 effect 会让隐私与权限审查失去有限边界，并允许未知字段或 action type 穿过 trusted boundary。

## 决策

两个边界都使用闭合、类型化 registry。Telemetry 字段只有通过已登记 source mapping 才能进入领域，只有 reviewed persistence manifest 中列出的 field ID 才能进入 SQLite。Raw request body、任意 attribute map、prompt、response、reasoning content、路径、账户或认证数据永远不跨越该边界。

产生 effect 的工作通过 `RegisteredActionRegistry` 解析，包含精确 action type、parameter-schema version、typed parameters、required capability 与 stored permission。未知字段、action type、schema version 和 dynamic parameter 失败关闭，没有 fallback executor。

## 影响

- 隐私审查使用有限的 machine-readable authority 与生成证据，不依赖 call-site 约定。
- 增加持久 telemetry 字段时，catalog、persistence manifest、storage 与生成参考必须完全一致。
- 增加或修改 action 时需要已登记的类型定义；语义改变时增加 schema version，并提供 bounded parameters 与显式 host executor。
- Generic payload capture 和临时 shell、plugin 或 string-dispatched action 不属于该架构。
