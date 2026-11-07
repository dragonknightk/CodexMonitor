# ADR 0001：Rust、Tauri 2 与 React 技术栈

[历史英文原件](0001-rust-tauri-react-stack.md)是不可变权威记录；本文件是中文伴随译本。

## 状态

2026-07-15 接受。

## 背景

CodexMonitor 需要长期运行的本机 engine、轻量跨平台 desktop host、原生 tray/notification 集成和类型化 Dashboard。运行时必须在不依赖用户机器上已安装解释器的前提下执行隐私和生命周期规则。用户界面需要能在普通浏览器中使用，并与未来可能加入的嵌入式视图共享合同。

## 决策

领域政策、运行时 adapter 和 desktop host 使用 Rust 实现。原生应用 shell 使用 Tauri 2，Dashboard 使用 React 与 TypeScript。领域政策放在 `codex-monitor-core`，运行时和协议 adapter 放在当时的 `codex-monitor-engine`，操作系统效果放在 Tauri host。Tauri 是生命周期和平台边界，不承担第二套应用服务。生产应用不分发或调用 Python runtime。

本段保留 2026-07-15 的历史 crate 表述；当前 adapter 边界由 [ADR 0007](0007-workspace-crate-boundaries.md) 进一步拆分。

## 影响

- 领域和运行时不变量编译进唯一生产二进制。
- Dashboard 可以使用 OpenAPI 生成类型，Rust engine 仍是运行时行为权威。
- 原生打包必须为每个支持平台构建并测试 Rust/Tauri target；前端开发使用固定 Node.js 与 pnpm toolchain。
- 无法纳入这些模块边界的平台效果或依赖需要新的架构决策，不能创建平行 runtime path。
