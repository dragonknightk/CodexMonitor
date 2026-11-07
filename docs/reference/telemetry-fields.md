<!-- 由 scripts/generate-telemetry-fields.mjs 生成。请勿直接编辑。 -->

# 遥测字段目录

权威来源为 `docs/_meta/telemetry-fields.yaml`。只有下方允许列表中的字段可以跨越
持久化边界。来源映射是明确的提取路径；它们不会授权通用属性映射或 JSON 映射。

目录：`telemetry-fields-v1`（模式版本 1）。

经隐私审查的 Codex 监控类型化数据语义允许列表。字段只有通过下方明确的来源映射 才会被接纳；解码后的载荷和任意属性映射绝不能跨越领域或持久化边界。

## 隐私分类

<!-- prettier-ignore -->
| 分类 | 含义 |
| --- | --- |
| `local_sensitive_label` | 为 Dashboard 按需解析、用户可见的本地标签；禁止持久化，也不得进入通知和诊断信息。 |
| `operational_metadata` | 用于解释模型调用、来源状态和本地监控行为的有界技术元数据；其中不含用户编写的内容。 |
| `pseudonymous_identifier` | 仅用于关联和去重的不透明本地标识符；它们仍属敏感信息，不得出现在通知或普通日志中。 |

## 保留策略

<!-- prettier-ignore -->
| 策略 | 是否持久化 | 规则 |
| --- | --- | --- |
| `request-memory-only` | 否 | 只在当前已认证的本地查询期间保留，并在响应后释放；绝不写入 SQLite、日志、告警或通知。 |
| `source-health-state` | 是 | 当前有界的来源健康状态以及安全时间戳和代码；已被取代的状态可以压缩，且不保留原始错误文本。 |
| `structured-monitoring` | 是 | 已存储的监控历史；默认保留 90 天，并遵循配置的 30 天、90 天、365 天或无限期保留策略。 |

## 允许字段

<!-- prettier-ignore -->
| 字段 ID | 领域实体 | 领域类型 | 可空 | 来源 | 隐私分类 | 是否持久化 | 保留策略 | 显示用途 | 安全边界 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `conversation.first_observed_at` | `Conversation` | `UtcTimestamp` | 否 | `domain-engine`<br>`legacy-import` | `operational_metadata` | 是 | `structured-monitoring` | 显示本地监控首次观测到会话的时间。 | 规范化为 UTC，并拒绝无效或无界的时间戳。 |
| `conversation.id` | `Conversation`<br>`Turn`<br>`ModelCall`<br>`ModelCallObservation` | `ConversationId` | 否 | `domain-engine` | `pseudonymous_identifier` | 是 | `structured-monitoring` | 将规范会话与轮次和模型调用关联起来。 | 只能生成内部 UUIDv7；不得将其与遥测来源提供的标识符混淆，也不得在系统通知中暴露。 |
| `conversation.last_observed_at` | `Conversation` | `UtcTimestamp` | 否 | `domain-engine`<br>`legacy-import` | `operational_metadata` | 是 | `structured-monitoring` | 按本地最近观测到的活动排序会话。 | 规范化为 UTC，并拒绝无效或无界的时间戳。 |
| `conversation.resolved_display_name` | `Conversation` | `EphemeralDisplayLabel` | 是 | `local-session-index` | `local_sensitive_label` | 否 | `request-memory-only` | 在已认证查询期间显示本地会话标签。 | 仅按需解析；绝不将标签复制到 SQLite、日志、告警或通知中。 |
| `conversation.source_id` | `Conversation`<br>`ModelCall`<br>`ModelCallObservation` | `ConversationSourceId` | 是 | `codex-sessions`<br>`legacy-import`<br>`otlp-logs` | `pseudonymous_identifier` | 是 | `structured-monitoring` | 将来源观测与规范会话关联起来。 | 只接受来源提供的有界不透明标识符；绝不记录到日志或放入系统通知。 |
| `conversation.thread_id` | `Conversation`<br>`ModelCall`<br>`ModelCallObservation` | `ThreadSourceId` | 是 | `codex-sessions`<br>`legacy-import`<br>`otlp-logs` | `pseudonymous_identifier` | 是 | `structured-monitoring` | 在已知时关联备用 Codex 任务标识。 | 将其视为可选的有界不透明标识符，并从日志和通知中省略。 |
| `model_call.cached_input_tokens` | `ModelCall`<br>`ModelCallObservation` | `TokenCount` | 是 | `codex-sessions`<br>`legacy-import`<br>`otlp-logs` | `operational_metadata` | 是 | `structured-monitoring` | 显示输入 token 用量中的缓存部分。 | 只接受经过验证的非负有界整数。 |
| `model_call.canonical_identity` | `ModelCall` | `SourceEventIdentity` | 否 | `domain-engine` | `pseudonymous_identifier` | 是 | `structured-monitoring` | 跨观测去重并关联规范模型调用。 | 只接受由允许列表字段派生且带版本的来源标识；标识中绝不能包含原始载荷或用户内容。 |
| `model_call.client` | `ModelCall`<br>`ModelCallObservation` | `ClientName` | 是 | `codex-sessions`<br>`legacy-import`<br>`otlp-logs` | `operational_metadata` | 是 | `structured-monitoring` | 在本地筛选器中区分受支持的 Codex 客户端。 | 只接受经过验证的有界客户端标签，不得包含主机或账户数据。 |
| `model_call.event_at` | `ModelCall`<br>`ModelCallObservation` | `UtcTimestamp` | 否 | `codex-sessions`<br>`legacy-import`<br>`otlp-logs` | `operational_metadata` | 是 | `structured-monitoring` | 排序模型调用并限定时间范围查询。 | 规范化为 UTC，并拒绝无效或无界的时间戳。 |
| `model_call.event_name` | `ModelCall`<br>`ModelCallObservation` | `EventName` | 是 | `codex-sessions`<br>`legacy-import`<br>`otlp-logs` | `operational_metadata` | 是 | `structured-monitoring` | 说明生成该观测的已注册 Codex 事件。 | 只接受 `codex.sse_event` 等已注册的有界事件标签。 |
| `model_call.id` | `ModelCall`<br>`ModelCallObservation` | `ModelCallId` | 否 | `domain-engine` | `pseudonymous_identifier` | 是 | `structured-monitoring` | 为调用、告警和去重提供稳定的本地标识。 | 只能根据已注册的来源标识规则生成；绝不从原始用户内容派生。 |
| `model_call.input_tokens` | `ModelCall`<br>`ModelCallObservation` | `TokenCount` | 是 | `codex-sessions`<br>`legacy-import`<br>`otlp-logs` | `operational_metadata` | 是 | `structured-monitoring` | 显示输入 token 用量并计算本地汇总。 | 只接受经过验证的非负有界整数。 |
| `model_call.model` | `ModelCall`<br>`ModelCallObservation` | `ModelName` | 是 | `codex-sessions`<br>`legacy-import`<br>`otlp-logs` | `operational_metadata` | 是 | `structured-monitoring` | 识别和筛选调用所使用的模型。 | 只接受经过验证且不含自由文本的有界模型标签。 |
| `model_call.output_tokens` | `ModelCall`<br>`ModelCallObservation` | `TokenCount` | 是 | `codex-sessions`<br>`legacy-import`<br>`otlp-logs` | `operational_metadata` | 是 | `structured-monitoring` | 显示输出 token 用量并计算本地汇总。 | 只接受经过验证的非负有界整数。 |
| `model_call.reasoning_effort` | `ModelCall`<br>`ModelCallObservation` | `ReasoningEffort` | 是 | `codex-sessions`<br>`legacy-import`<br>`local-codex-config`<br>`otlp-logs` | `operational_metadata` | 是 | `structured-monitoring` | 在已知时说明请求的推理配置。 | 只接受已注册的枚举值，并保留该值是直接取得还是推断所得的信息。 |
| `model_call.reasoning_output_tokens` | `ModelCall`<br>`ModelCallObservation` | `TokenCount` | 是 | `codex-sessions`<br>`legacy-import`<br>`otlp-logs` | `operational_metadata` | 是 | `structured-monitoring` | 显示推理 token 用量并评估已注册的 token 规则。 | 只接受经过验证的非负有界整数；不得根据计数推断隐藏的推理内容。 |
| `model_call.stream_event_kind` | `ModelCall`<br>`ModelCallObservation` | `StreamEventKind` | 是 | `codex-sessions`<br>`legacy-import`<br>`otlp-logs` | `operational_metadata` | 是 | `structured-monitoring` | 区分 `response.completed` 与其他已注册的流事件。 | 只接受经过验证且不含正文文本的有界流事件标签。 |
| `model_call.surface` | `ModelCall`<br>`ModelCallObservation` | `SurfaceName` | 是 | `codex-sessions`<br>`legacy-import`<br>`otlp-logs` | `operational_metadata` | 是 | `structured-monitoring` | 区分 Desktop、CLI 和 IDE 监控界面。 | 只接受经过验证的有界界面标签。 |
| `model_call.temporal_class` | `ModelCall`<br>`ModelCallObservation` | `DataOrigin` | 否 | `domain-engine`<br>`legacy-import` | `operational_metadata` | 是 | `structured-monitoring` | 区分实时调用与导入或已有的历史记录。 | 只接受 `realtime` 或 `historical` 枚举值；历史值不得进入实时动作管线。 |
| `model_call_observation.field_provenance` | `ModelCallObservation` | `FieldProvenance` | 否 | `source-adapter` | `operational_metadata` | 是 | `structured-monitoring` | 说明每个类型化值使用的已注册来源路径。 | 只存储已注册的溯源枚举值或目录字段引用，不得存储任意来源键或属性映射。 |
| `model_call_observation.field_quality` | `ModelCallObservation` | `EvidenceQuality` | 否 | `source-adapter` | `operational_metadata` | 是 | `structured-monitoring` | 区分直接证据和推断值，以确定优先级。 | 只接受已注册的直接或推断质量枚举值。 |
| `model_call_observation.id` | `ModelCallObservation` | `ModelCallObservationId` | 否 | `domain-engine` | `pseudonymous_identifier` | 是 | `structured-monitoring` | 为每条已接纳的来源观测提供稳定的本地标识。 | 只能从已注册的安全来源标识组成部分派生。 |
| `model_call_observation.observed_at` | `ModelCallObservation` | `UtcTimestamp` | 否 | `codex-sessions`<br>`legacy-import`<br>`otlp-logs` | `operational_metadata` | 是 | `structured-monitoring` | 审计本地来源适配器观测到证据的时间。 | 在本地生成经过验证的 UTC 时间戳。 |
| `model_call_observation.source_contract_version` | `ModelCallObservation` | `SourceContractVersion` | 否 | `source-adapter` | `operational_metadata` | 是 | `structured-monitoring` | 保留定义该观测的来源契约。 | 只接受已注册的正整数来源契约版本。 |
| `model_call_observation.source_event_identity` | `ModelCallObservation` | `SourceEventIdentity` | 否 | `codex-sessions`<br>`domain-engine`<br>`legacy-import`<br>`otlp-logs` | `pseudonymous_identifier` | 是 | `structured-monitoring` | 以确定性方式去重重传的来源事件。 | 使用有界稳定的来源 ID，或仅由目录所列元数据组成的指纹；不得将原始载荷或用户内容哈希进标识。 |
| `model_call_observation.source_event_identity_version` | `ModelCallObservation` | `IdentityAlgorithmVersion` | 否 | `domain-engine` | `operational_metadata` | 是 | `structured-monitoring` | 保留来源标识的历史语义。 | 只接受受支持的正整数标识算法版本。 |
| `model_call_observation.source_id` | `ModelCallObservation` | `SourceId` | 否 | `source-adapter` | `operational_metadata` | 是 | `structured-monitoring` | 将证据归属到生成它的适配器。 | 只接受 engine 已注册的来源 ID。 |
| `source_health.last_accepted_at` | `SourceHealth` | `UtcTimestamp` | 是 | `domain-engine` | `operational_metadata` | 是 | `source-health-state` | 显示来源最近一次接纳安全类型化证据的时间。 | 规范化为 UTC，且只在类型化提取被接纳后更新。 |
| `source_health.last_error_code` | `SourceHealth` | `SafeErrorCode` | 是 | `domain-engine` | `operational_metadata` | 是 | `source-health-state` | 提供不含载荷细节且可执行的接收器诊断信息。 | 只存储已注册的稳定代码；丢弃原始错误、路径、标识符和载荷摘录。 |
| `source_health.last_error_occurred_at` | `SourceHealth` | `UtcTimestamp` | 是 | `domain-engine` | `operational_metadata` | 是 | `source-health-state` | 显示最近一次隐私安全的来源故障发生时间。 | 规范化为 UTC，且只能与已注册的安全错误代码一同持久化；不得保留错误链或载荷摘录。 |
| `source_health.source_contract_version` | `SourceHealth` | `SourceContractVersion` | 否 | `domain-engine` | `operational_metadata` | 是 | `source-health-state` | 显示拥有该健康状态的已注册来源契约。 | 只接受已注册的正整数来源契约版本。 |
| `source_health.source_id` | `SourceHealth` | `SourceId` | 否 | `domain-engine` | `operational_metadata` | 是 | `source-health-state` | 标识正在显示接收器或扫描器健康状态的来源。 | 只接受 engine 已注册的来源 ID。 |
| `source_health.status` | `SourceHealth` | `SourceHealthStatus` | 否 | `domain-engine` | `operational_metadata` | 是 | `source-health-state` | 显示来源处于健康、降级还是不可用状态。 | 只接受已注册的有界状态枚举值。 |
| `source_health.supported_fields` | `SourceHealth` | `FieldIdSet` | 否 | `domain-engine` | `operational_metadata` | 是 | `source-health-state` | 报告该来源能够安全提供的目录字段。 | 只接受本目录已注册字段 ID 的有界集合；不得将任意来源键复制进该集合。 |
| `source_health.updated_at` | `SourceHealth` | `UtcTimestamp` | 否 | `domain-engine` | `operational_metadata` | 是 | `source-health-state` | 显示当前来源健康状态最近更新时间。 | 在本地生成经过验证的 UTC 时间戳。 |
| `turn.event_at` | `Turn` | `UtcTimestamp` | 是 | `codex-sessions`<br>`domain-engine` | `operational_metadata` | 是 | `structured-monitoring` | 将已证明的轮次放到本地会话时间线上。 | 只存储来自已证明轮次证据且经过验证的 UTC 时间戳。 |
| `turn.id` | `Turn`<br>`ModelCall`<br>`ModelCallObservation` | `TurnId` | 否 | `domain-engine` | `pseudonymous_identifier` | 是 | `structured-monitoring` | 将规范轮次与其会话和模型调用关联起来。 | 仅在轮次关系得到证明后生成内部 UUIDv7；不得替换为来源标识符，也不得仅凭时间推断轮次。 |
| `turn.source_id` | `Turn`<br>`ModelCall`<br>`ModelCallObservation` | `TurnSourceId` | 是 | `codex-sessions` | `pseudonymous_identifier` | 是 | `structured-monitoring` | 将来源轮次证据与规范轮次关联起来。 | 不存在可靠来源关系时保留未知值；不得仅凭时间虚构来源轮次标识符。 |

## 明确的来源映射

这里列出了每个可接受的同义键。来源适配器必须先丢弃未列出的键，
然后才能构造类型化领域值。

<!-- prettier-ignore -->
| 字段 ID | 来源 | 接受的映射 |
| --- | --- | --- |
| `conversation.first_observed_at` | `domain-engine` | `derived:min(model_call_observation.observed_at)` |
| `conversation.first_observed_at` | `legacy-import` | `legacy.codex_conversations.first_seen_at` |
| `conversation.id` | `domain-engine` | `derived:canonical_conversation_identity` |
| `conversation.last_observed_at` | `domain-engine` | `derived:max(model_call_observation.observed_at)` |
| `conversation.last_observed_at` | `legacy-import` | `legacy.codex_conversations.last_seen_at` |
| `conversation.resolved_display_name` | `local-session-index` | `session_index.thread_name_by_conversation_id`<br>`session_index.thread_name_by_thread_id` |
| `conversation.source_id` | `codex-sessions` | `session.conversation_id` |
| `conversation.source_id` | `legacy-import` | `legacy.codex_conversations.conversation_id`<br>`legacy.codex_token_events.conversation_id` |
| `conversation.source_id` | `otlp-logs` | `log_record.attributes["codex.conversation_id"]`<br>`log_record.attributes["conversation.id"]`<br>`log_record.attributes["conversation_id"]`<br>`log_record.body.response.conversation` |
| `conversation.thread_id` | `codex-sessions` | `session.thread_id` |
| `conversation.thread_id` | `legacy-import` | `legacy.codex_token_events.thread_id` |
| `conversation.thread_id` | `otlp-logs` | `log_record.attributes["codex.thread_id"]`<br>`log_record.attributes["session.id"]`<br>`log_record.attributes["session_id"]`<br>`log_record.attributes["thread.id"]`<br>`log_record.attributes["thread_id"]` |
| `model_call.cached_input_tokens` | `codex-sessions` | `session.model_call.usage.cached_input_tokens` |
| `model_call.cached_input_tokens` | `legacy-import` | `legacy.codex_token_events.cached_input_tokens` |
| `model_call.cached_input_tokens` | `otlp-logs` | `log_record.attributes["cached_input_tokens"]`<br>`log_record.attributes["cached_token_count"]`<br>`log_record.body.response.usage.cached_input_tokens`<br>`log_record.body.response.usage.input_tokens_details.cached_tokens` |
| `model_call.canonical_identity` | `domain-engine` | `derived:canonical_source_event_identity` |
| `model_call.client` | `codex-sessions` | `session.client` |
| `model_call.client` | `legacy-import` | `legacy.codex_token_events.client` |
| `model_call.client` | `otlp-logs` | `log_record.attributes["app"]`<br>`log_record.attributes["app.name"]`<br>`log_record.attributes["client"]`<br>`log_record.attributes["client.name"]`<br>`resource.attributes["service.name"]` |
| `model_call.event_at` | `codex-sessions` | `session.event.timestamp` |
| `model_call.event_at` | `legacy-import` | `legacy.codex_token_events.created_at` |
| `model_call.event_at` | `otlp-logs` | `log_record.observed_time_unix_nano`<br>`log_record.time_unix_nano` |
| `model_call.event_name` | `codex-sessions` | `session.event.name` |
| `model_call.event_name` | `legacy-import` | `legacy.codex_token_events.raw_event_name` |
| `model_call.event_name` | `otlp-logs` | `log_record.attributes["event.name"]`<br>`log_record.attributes["event_name"]`<br>`log_record.attributes["log.name"]`<br>`log_record.attributes["name"]`<br>`log_record.attributes["otel.event.name"]` |
| `model_call.id` | `domain-engine` | `derived:canonical_model_call_identity` |
| `model_call.input_tokens` | `codex-sessions` | `session.model_call.usage.input_tokens` |
| `model_call.input_tokens` | `legacy-import` | `legacy.codex_token_events.input_tokens` |
| `model_call.input_tokens` | `otlp-logs` | `log_record.attributes["input_token_count"]`<br>`log_record.attributes["input_tokens"]`<br>`log_record.body.response.usage.input_tokens` |
| `model_call.model` | `codex-sessions` | `session.model_call.model` |
| `model_call.model` | `legacy-import` | `legacy.codex_token_events.model` |
| `model_call.model` | `otlp-logs` | `log_record.attributes["gen_ai.request.model"]`<br>`log_record.attributes["gen_ai.response.model"]`<br>`log_record.attributes["model"]`<br>`log_record.attributes["model_name"]`<br>`log_record.attributes["openai.model"]`<br>`log_record.body.response.model` |
| `model_call.output_tokens` | `codex-sessions` | `session.model_call.usage.output_tokens` |
| `model_call.output_tokens` | `legacy-import` | `legacy.codex_token_events.output_tokens` |
| `model_call.output_tokens` | `otlp-logs` | `log_record.attributes["output_token_count"]`<br>`log_record.attributes["output_tokens"]`<br>`log_record.body.response.usage.output_tokens` |
| `model_call.reasoning_effort` | `codex-sessions` | `session.model_call.reasoning_effort` |
| `model_call.reasoning_effort` | `legacy-import` | `legacy.codex_token_events.reasoning_effort` |
| `model_call.reasoning_effort` | `local-codex-config` | `config.model_reasoning_effort` |
| `model_call.reasoning_effort` | `otlp-logs` | `log_record.attributes["model_reasoning_effort"]`<br>`log_record.attributes["openai.reasoning_effort"]`<br>`log_record.attributes["reasoning.effort"]`<br>`log_record.attributes["reasoning_effort"]` |
| `model_call.reasoning_output_tokens` | `codex-sessions` | `session.model_call.usage.reasoning_output_tokens` |
| `model_call.reasoning_output_tokens` | `legacy-import` | `legacy.codex_token_events.reasoning_output_tokens` |
| `model_call.reasoning_output_tokens` | `otlp-logs` | `log_record.attributes["reasoning_output_tokens"]`<br>`log_record.attributes["reasoning_token_count"]`<br>`log_record.body.response.usage.output_tokens_details.reasoning_tokens`<br>`log_record.body.response.usage.reasoning_output_tokens` |
| `model_call.stream_event_kind` | `codex-sessions` | `session.model_call.stream_event_kind` |
| `model_call.stream_event_kind` | `legacy-import` | `legacy.codex_token_events.stream_event_kind` |
| `model_call.stream_event_kind` | `otlp-logs` | `log_record.attributes["event.kind"]`<br>`log_record.attributes["kind"]`<br>`log_record.attributes["response.type"]`<br>`log_record.attributes["sse_event_kind"]`<br>`log_record.attributes["stream_event_kind"]`<br>`log_record.attributes["type"]`<br>`log_record.body.type` |
| `model_call.surface` | `codex-sessions` | `session.surface` |
| `model_call.surface` | `legacy-import` | `legacy.codex_token_events.surface` |
| `model_call.surface` | `otlp-logs` | `log_record.attributes["app.surface"]`<br>`log_record.attributes["codex.surface"]`<br>`log_record.attributes["originator"]`<br>`log_record.attributes["surface"]` |
| `model_call.temporal_class` | `domain-engine` | `ingestion_context.data_origin` |
| `model_call.temporal_class` | `legacy-import` | `derived:historical` |
| `model_call_observation.field_provenance` | `source-adapter` | `typed_extractor.registered_field_provenance` |
| `model_call_observation.field_quality` | `source-adapter` | `typed_extractor.field_quality` |
| `model_call_observation.id` | `domain-engine` | `derived:observation_identity` |
| `model_call_observation.observed_at` | `codex-sessions` | `scanner.observed_at_utc` |
| `model_call_observation.observed_at` | `legacy-import` | `import.observed_at_utc` |
| `model_call_observation.observed_at` | `otlp-logs` | `receiver.observed_at_utc` |
| `model_call_observation.source_contract_version` | `source-adapter` | `source_event_identity.source.contract_version` |
| `model_call_observation.source_event_identity` | `codex-sessions` | `session.event.id` |
| `model_call_observation.source_event_identity` | `domain-engine` | `derived:versioned_allowlisted_fingerprint` |
| `model_call_observation.source_event_identity` | `legacy-import` | `derived:versioned_legacy_row_identity` |
| `model_call_observation.source_event_identity` | `otlp-logs` | `log_record.attributes["response.id"]`<br>`log_record.body.response.id`<br>`log_record.span_id`<br>`log_record.trace_id` |
| `model_call_observation.source_event_identity_version` | `domain-engine` | `source_event_identity.algorithm_version` |
| `model_call_observation.source_id` | `source-adapter` | `source_event_identity.source.id` |
| `source_health.last_accepted_at` | `domain-engine` | `source_runtime.last_accepted_at_utc` |
| `source_health.last_error_code` | `domain-engine` | `source_runtime.last_failure.code` |
| `source_health.last_error_occurred_at` | `domain-engine` | `source_runtime.last_failure.occurred_at` |
| `source_health.source_contract_version` | `domain-engine` | `source_runtime.identity.contract_version` |
| `source_health.source_id` | `domain-engine` | `source_runtime.identity.id` |
| `source_health.status` | `domain-engine` | `source_runtime.status` |
| `source_health.supported_fields` | `domain-engine` | `source_runtime.supported_fields` |
| `source_health.updated_at` | `domain-engine` | `source_runtime.health_updated_at_utc` |
| `turn.event_at` | `codex-sessions` | `session.turn.event_at` |
| `turn.event_at` | `domain-engine` | `correlation.proven_turn_event_at` |
| `turn.id` | `domain-engine` | `correlation.proven_turn_id`<br>`derived:canonical_turn_identity` |
| `turn.source_id` | `codex-sessions` | `session.turn.id` |

## 永久禁止字段

禁止字段不属于“允许但不持久化”的条目。它们会在进入类型化领域前被拒绝；
若不修改这份隐私权威，就不能添加这些字段。

<!-- prettier-ignore -->
| 禁止规则 | 禁止的字段 ID | 禁止的 ID 词项 | 来源键 | 原因 | 强制措施 |
| --- | --- | --- | --- | --- | --- |
| `authentication_material` | `auth.api_key`<br>`auth.cookie`<br>`auth.token`<br>`auth.authorization` | `api_key`<br>`cookie`<br>`auth_token`<br>`authorization` | `api.key`<br>`api_key`<br>`cookie`<br>`auth.token`<br>`authorization` | 认证材料属于秘密信息，不具备监控或显示用途。 | 立即丢弃，绝不能纳入错误、日志、存储或输出。 |
| `commands` | `command.text`<br>`command.shell`<br>`command.executable` | `command`<br>`command_text`<br>`shell_command` | `command`<br>`shell.command`<br>`executable` | 命令属于用户或工具内容，绝不能成为已存储或可执行的数据。 | 在构造领域值之前丢弃，绝不转发给进程 API。 |
| `personal_accounts` | `identity.email`<br>`identity.account_id`<br>`identity.user_id` | `email`<br>`account_id`<br>`user_id` | `user.email`<br>`account.id`<br>`account_id`<br>`user.id` | 本地监控不需要电子邮件、账户和用户标识符。 | 在构造类型化领域值、记录日志和存储之前丢弃。 |
| `raw_payloads` | `raw.body`<br>`raw.json`<br>`raw.payload` | `raw_body`<br>`raw_json`<br>`raw_payload`<br>`generic_json` | `log_record.body`<br>`raw_json`<br>`raw_payload`<br>`payload` | 原始遥测容器可能携带当前或未来的任意用户内容，绝不是有效的领域值或持久化值。 | 在有界内存中解码，提取已注册的类型化路径，然后丢弃。 |
| `reasoning_content` | `reasoning.summary`<br>`reasoning.content`<br>`reasoning.encrypted` | `reasoning_summary`<br>`reasoning_content`<br>`encrypted_reasoning` | `reasoning.summary`<br>`reasoning.content`<br>`encrypted_reasoning`<br>`reasoning.encrypted_content` | 隐藏、摘要化或加密的推理内容绝不属于监控数据。 | 在构造类型化领域值、记录日志、生成告警和存储之前丢弃。 |
| `tool_data` | `tool.arguments`<br>`tool.output`<br>`tool.data` | `tool_arguments`<br>`tool_output`<br>`tool_data` | `tool.arguments`<br>`tool.output`<br>`arguments`<br>`command_output` | 工具参数和输出可能包含源代码、秘密或本地数据。 | 在构造类型化领域值、记录日志、生成告警和存储之前丢弃。 |
| `user_content` | `content.prompt`<br>`content.response_text`<br>`content.user_message`<br>`content.assistant_message` | `prompt`<br>`response_text`<br>`user_message`<br>`assistant_message` | `prompt`<br>`user.prompt`<br>`response.text`<br>`output_text`<br>`input_text` | 提示词、响应和消息文本不属于监控目的。 | 在构造类型化领域值、记录日志、生成告警和存储之前丢弃。 |
| `workspace_paths` | `workspace.path`<br>`filesystem.path`<br>`file.path` | `workspace_path`<br>`filesystem_path`<br>`file_path` | `workspace.path`<br>`cwd`<br>`file.path`<br>`filesystem.path` | 本地路径会暴露与 token 监控无关的用户和工作区信息。 | 在构造类型化领域值、诊断和存储之前丢弃。 |

## 持久化验证

持久化实现会导出版本 1 的 YAML 或 JSON 清单，其中含有
`persisted_field_ids` 列表。请验证它与目录中 `persisted` 为 `true` 的
全部字段完全一致：

```text
node scripts/generate-telemetry-fields.mjs --check --persistence-manifest <path>
```

缺失字段 ID 或出现未注册字段 ID 都会使命令失败。
