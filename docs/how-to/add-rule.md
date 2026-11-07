# 增加 rule

Rule 必须已登记、带版本且类型化。它只求值 durable domain event，可以请求已登记 action，不能直接执行 effect。

定义稳定 rule ID、整数 version，以及只含已登记 field/operator 的 schema。持久 evaluation identity 由 rule ID、version 与 model call 组成，重复交付不能重复创建 alert。Match 保存紧凑结构化 evidence，失败保存闭合 safe error code。

明确 historical record 是否 eligible。Realtime rule 必须排除 import 和预先存在的 call；需要历史分析时建立独立的用户请求操作。边界测试覆盖 exact match、neighboring value、missing value、重复交付、重启和 version change。

修改 `codex-monitor-core` 的 registry 与 `codex-monitor-engine` 的 worker 时，同时运行对应局部测试和 product integration；语义变化必须增加版本，不能修改现有 fingerprint。
