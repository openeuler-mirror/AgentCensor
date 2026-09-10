//! Process-tree snapshot contracts for attach bootstrap.

use std::time::SystemTime;

use model_core::process::ProcessObservation;

/// One process and its parent as observed during attach bootstrap.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessSnapshot {
    pub identity: ProcessObservation,
    pub parent: Option<ProcessObservation>,
    pub executable: Option<String>,
    pub current_working_directory: Option<String>,
}

/// Point-in-time process tree rooted at the PID passed to `track-add`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreeSnapshot {
    pub root: ProcessObservation,
    pub captured_at: SystemTime,
    pub processes: Vec<ProcessSnapshot>,
}

impl TreeSnapshot {
    pub fn root_working_directory(&self) -> Option<&str> {
        self.processes
            .iter()
            .find(|process| process.identity == self.root)
            .and_then(|process| process.current_working_directory.as_deref())
    }
}

/// Adapter contract for constructing an initial process tree.
pub trait ProcessTreeSnapshotter {
    type Error;

    /// Captures the root and all descendants visible at attach time.
    fn snapshot(&self, root: &ProcessObservation) -> Result<TreeSnapshot, Self::Error>;
}
