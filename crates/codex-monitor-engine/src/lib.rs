//! `CodexMonitor` 的 worker、适配器桥接与常驻组合根。

pub mod actions;
mod bridges;
pub mod rules;
pub mod runtime;
mod settings;

pub use settings::{
    LoginAutostartError, LoginAutostartHost, SettingsMonitor, SettingsMonitorError,
    SettingsMutation, SettingsRuntime, SettingsRuntimeCreateError, SettingsRuntimeError,
};

pub use codex_monitor_core::OTLP_SOURCE_ID;
