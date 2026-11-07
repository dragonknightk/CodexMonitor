# CodexMonitor 文档

本页是统一的任务导航。简体中文是内部技术文档和用户文档的权威语言；英文用户文档是完整翻译。代码、命令、路径、配置键和协议字段保留原始英文拼写。

## 安装和使用打包应用

- 中文权威版本：[README](../README.md)、[安装](user/installation.zh-CN.md)、
  [升级](user/upgrade.zh-CN.md)、[卸载与数据删除](user/uninstall.zh-CN.md)、
  [故障排查](user/troubleshooting.zh-CN.md)和[隐私说明](../PRIVACY.zh-CN.md)。
- 英文翻译：[README](../README.en.md)、[安装](user/installation.md)、
  [升级](user/upgrade.md)、[卸载与数据删除](user/uninstall.md)、
  [故障排查](user/troubleshooting.md)和[隐私说明](../PRIVACY.md)。

## 了解系统

- [系统概览](architecture/system-overview.md)：进程、crate、网络和隐私边界。
- [常驻生命周期](architecture/process-lifecycle.md)：启动、后台行为与有序关闭。
- [监控领域模型](domain/monitoring-model.md)：conversation、turn、model call、observation 与 provenance。

## 扩展产品

- [增加 telemetry 来源](how-to/add-telemetry-source.md)
- [增加监控字段](how-to/add-telemetry-field.md)
- [增加规则](how-to/add-rule.md)
- [增加动作](how-to/add-action.md)
- [增加界面语言](how-to/add-locale.md)
- [增加数据库迁移](how-to/add-database-migration.md)

## 开发与验证

- [标准命令](development/commands.md)
- [测试策略](development/testing.md)
- [原生打包与发布](development/releasing.md)
- [旧 OTLP 合同切换](development/legacy-contract-cutover.md)
- [CodexMonitor v1 能力追踪](development/capability-traceability.md)
- [安全、供应链与隐私审计](development/security-and-privacy-audits.md)
- [文档工作流](development/documentation-workflow.md)
- [合成 OTLP fixture](development/fixtures.md)

## 查阅合同、决策与规范

- [已登记 telemetry 来源](reference/telemetry-sources.md)
- [telemetry 字段目录](reference/telemetry-fields.md)
- [数据库 schema](reference/database-schema.md)
- [架构决策记录](decisions/README.md)
