use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum HostStartupState {
    Starting,
    CancelRequested,
    Ready,
    Cancelled,
    Failed,
}

struct HostLifecycleInner {
    startup: AtomicU8,
    shutdown_started: AtomicBool,
}

/// 可在不初始化 Tauri event loop 的情况下验证的宿主生命周期状态机。
#[derive(Clone)]
pub struct HostLifecycleStateMachine {
    inner: Arc<HostLifecycleInner>,
}

impl Default for HostLifecycleStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl HostLifecycleStateMachine {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(HostLifecycleInner {
                startup: AtomicU8::new(HostStartupState::Starting as u8),
                shutdown_started: AtomicBool::new(false),
            }),
        }
    }

    #[must_use]
    pub fn startup_state(&self) -> HostStartupState {
        match self.inner.startup.load(Ordering::Acquire) {
            value if value == HostStartupState::Starting as u8 => HostStartupState::Starting,
            value if value == HostStartupState::CancelRequested as u8 => {
                HostStartupState::CancelRequested
            }
            value if value == HostStartupState::Ready as u8 => HostStartupState::Ready,
            value if value == HostStartupState::Cancelled as u8 => HostStartupState::Cancelled,
            _ => HostStartupState::Failed,
        }
    }

    pub fn claim_ready(&self) -> bool {
        self.inner
            .startup
            .compare_exchange(
                HostStartupState::Starting as u8,
                HostStartupState::Ready as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub fn request_startup_cancel(&self) -> bool {
        self.inner
            .startup
            .compare_exchange(
                HostStartupState::Starting as u8,
                HostStartupState::CancelRequested as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub fn mark_cancelled(&self) {
        self.inner
            .startup
            .store(HostStartupState::Cancelled as u8, Ordering::Release);
    }

    pub fn mark_failed(&self) {
        self.inner
            .startup
            .store(HostStartupState::Failed as u8, Ordering::Release);
    }

    pub fn begin_shutdown(&self) -> bool {
        !self.inner.shutdown_started.swap(true, Ordering::AcqRel)
    }

    #[must_use]
    pub fn shutdown_started(&self) -> bool {
        self.inner.shutdown_started.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_loses_to_cancellation_and_shutdown_is_idempotent() {
        let lifecycle = HostLifecycleStateMachine::new();
        assert!(lifecycle.request_startup_cancel());
        assert!(!lifecycle.claim_ready());
        lifecycle.mark_cancelled();
        assert_eq!(lifecycle.startup_state(), HostStartupState::Cancelled);
        assert!(lifecycle.begin_shutdown());
        assert!(!lifecycle.begin_shutdown());
        assert!(lifecycle.shutdown_started());
    }

    #[test]
    fn ready_and_failed_states_are_closed() {
        let ready = HostLifecycleStateMachine::new();
        assert!(ready.claim_ready());
        assert_eq!(ready.startup_state(), HostStartupState::Ready);

        let failed = HostLifecycleStateMachine::new();
        failed.mark_failed();
        assert_eq!(failed.startup_state(), HostStartupState::Failed);
    }
}
