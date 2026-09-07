use crate::error::{ErrorCode, Result, CensorFsError};
use parking_lot::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Deterministic persistence fault injection used by crash/restart tests.
#[derive(Debug, Default)]
pub struct FaultInjector {
    fail_at: AtomicUsize,
    seen: AtomicUsize,
    labels: Mutex<Vec<String>>,
}

pub type FaultInjectBackend = FaultInjector;

impl FaultInjector {
    pub fn disabled() -> Self {
        Self::default()
    }

    pub fn fail_at(step: usize) -> Self {
        Self {
            fail_at: AtomicUsize::new(step),
            ..Self::default()
        }
    }

    pub fn reset(&self, fail_at: usize) {
        self.fail_at.store(fail_at, Ordering::SeqCst);
        self.seen.store(0, Ordering::SeqCst);
        self.labels.lock().clear();
    }

    pub fn checkpoint(&self, label: &str) -> Result<()> {
        self.labels.lock().push(label.to_owned());
        let step = self.seen.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_at.load(Ordering::SeqCst) == step {
            return Err(CensorFsError::new(
                ErrorCode::IoError,
                format!("injected crash at {label}"),
            ));
        }
        Ok(())
    }

    pub fn seen(&self) -> usize {
        self.seen.load(Ordering::SeqCst)
    }
    pub fn labels(&self) -> Vec<String> {
        self.labels.lock().clone()
    }
}
