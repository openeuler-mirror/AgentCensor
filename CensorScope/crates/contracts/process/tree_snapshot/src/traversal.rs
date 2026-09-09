//! Traversal contracts for deterministic snapshot processing.

use super::snapshot::{ProcessSnapshot, TreeSnapshot};

/// Ordering requested when consuming a process tree snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SnapshotTraversalOrder {
    RootFirst,
    ParentBeforeChild,
}

/// Deterministic traversal of a captured process tree.
pub trait SnapshotTraversal {
    /// Returns snapshot entries in the requested deterministic order.
    fn ordered<'a>(
        &'a self,
        snapshot: &'a TreeSnapshot,
        order: SnapshotTraversalOrder,
    ) -> Vec<&'a ProcessSnapshot>;
}
