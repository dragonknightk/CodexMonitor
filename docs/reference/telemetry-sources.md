<!-- 由 scripts/generate-references.mjs 生成。请勿直接编辑。 -->
# 已注册的遥测来源

| 来源 ID | 契约版本 | v1 状态 | 能力 |
| --- | ---: | --- | --- |
| `otlp-logs` | 1 | `full-v1` | 已完成调用的 token 元数据; 模型和推理强度; 会话标识; 来源健康状态 |
| `legacy-import` | 1 | `full-v1` | 历史已完成调用的 token 元数据; 推断的模型和推理强度; 会话标识和生命周期元数据; 隐私安全的导入审计 |
| `codex-sessions` | 1 | `reserved-unregistered` | 未来的轮次关系; 检查点; 会话元数据 |

session_index.jsonl 是可选的只读标签解析器，不属于遥测来源。
解析得到的会话名称绝不会复制到持久化监控表。
