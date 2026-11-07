//! 与 Tauri 解耦的常驻宿主协调层。

mod coordinator;
mod lifecycle;
mod startup;
mod tray;

pub use coordinator::*;
pub use lifecycle::*;
pub use startup::*;
pub use tray::*;
