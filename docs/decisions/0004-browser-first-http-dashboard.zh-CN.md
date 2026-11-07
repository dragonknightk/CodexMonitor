# ADR 0004：Browser-first HTTP Dashboard

[历史英文原件](0004-browser-first-http-dashboard.md)是不可变权威记录；本文件是中文伴随译本。

## 状态

2026-07-15 接受。

## 背景

Tauri 可以通过原生 WebView 与 JavaScript IPC 暴露应用行为，但这会在 HTTP API 旁增加一套 privileged UI-only protocol。本机监控 Dashboard 的完整合同在普通浏览器中工作时，更容易测试、检查与复用。

## 决策

React Dashboard 和所有 v1 业务操作由 loopback Dashboard listener 通过 HTTP REST 与 SSE 提供。Tauri host 不配置原生 window，而是使用短期单次 bootstrap URL 打开默认浏览器。Dashboard 不依赖 Tauri JavaScript bridge。未来嵌入式 WebView 必须加载同一本机 origin 并使用同一 authenticated HTTP contract，不能增加第二条业务 IPC path。

## 影响

- Browser 与未来嵌入式 client 使用同一套 OpenAPI-governed contract。
- HTTP service 必须提供 bootstrap credential、bounded session、CSRF protection、精确 host/origin validation 和显式 SSE resynchronization。
- 原生效果保留在类型化 engine-to-host port 后面，frontend JavaScript 不能直接调用。
- 打开系统浏览器属于 host 职责；Dashboard 行为可以在没有原生 window 的环境中开发和测试。
