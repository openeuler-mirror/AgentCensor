//! Trace-scoped file-descriptor lineage for pipes and Unix socket pairs.

use std::collections::BTreeMap;

use model_core::ids::TraceId;
use model_core::process::ProcessIdentity;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FdKey {
    pub trace_id: TraceId,
    pub process: ProcessIdentity,
    pub fd: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FdChannelKind {
    Unknown,
    Pipe,
    UnixSocket,
    File,
    Socket,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FdState {
    pub channel_id: u64,
    pub kind: FdChannelKind,
    pub peer_fd: Option<i32>,
}

#[derive(Clone, Debug, Default)]
pub struct FdLineage {
    next_channel_id: u64,
    entries: BTreeMap<FdKey, FdState>,
}

impl FdLineage {
    pub fn new() -> Self {
        Self {
            next_channel_id: 1,
            entries: BTreeMap::new(),
        }
    }

    pub fn insert(&mut self, key: FdKey, kind: FdChannelKind) -> FdState {
        let state = FdState {
            channel_id: self.allocate_channel(),
            kind,
            peer_fd: None,
        };
        self.entries.insert(key, state);
        state
    }

    pub fn pipe_pair(
        &mut self,
        trace_id: TraceId,
        process: ProcessIdentity,
        read_fd: i32,
        write_fd: i32,
    ) {
        let channel_id = self.allocate_channel();
        self.entries.insert(
            FdKey {
                trace_id,
                process,
                fd: read_fd,
            },
            FdState {
                channel_id,
                kind: FdChannelKind::Pipe,
                peer_fd: Some(write_fd),
            },
        );
        self.entries.insert(
            FdKey {
                trace_id,
                process,
                fd: write_fd,
            },
            FdState {
                channel_id,
                kind: FdChannelKind::Pipe,
                peer_fd: Some(read_fd),
            },
        );
    }

    pub fn socket_pair(
        &mut self,
        trace_id: TraceId,
        process: ProcessIdentity,
        first_fd: i32,
        second_fd: i32,
    ) {
        let channel_id = self.allocate_channel();
        self.entries.insert(
            FdKey {
                trace_id,
                process,
                fd: first_fd,
            },
            FdState {
                channel_id,
                kind: FdChannelKind::UnixSocket,
                peer_fd: Some(second_fd),
            },
        );
        self.entries.insert(
            FdKey {
                trace_id,
                process,
                fd: second_fd,
            },
            FdState {
                channel_id,
                kind: FdChannelKind::UnixSocket,
                peer_fd: Some(first_fd),
            },
        );
    }

    pub fn dup(
        &mut self,
        trace_id: TraceId,
        process: ProcessIdentity,
        source_fd: i32,
        target_fd: i32,
    ) -> Option<FdState> {
        let state = self
            .entries
            .get(&FdKey {
                trace_id,
                process,
                fd: source_fd,
            })
            .copied()?;
        self.entries.insert(
            FdKey {
                trace_id,
                process,
                fd: target_fd,
            },
            state,
        );
        Some(state)
    }

    pub fn fork(&mut self, trace_id: TraceId, parent: ProcessIdentity, child: ProcessIdentity) {
        let inherited = self
            .entries
            .iter()
            .filter(|(key, _)| key.trace_id == trace_id && key.process == parent)
            .map(|(key, state)| (key.fd, *state))
            .collect::<Vec<_>>();
        for (fd, state) in inherited {
            self.entries.insert(
                FdKey {
                    trace_id,
                    process: child,
                    fd,
                },
                state,
            );
        }
    }

    pub fn close(
        &mut self,
        trace_id: TraceId,
        process: ProcessIdentity,
        fd: i32,
    ) -> Option<FdState> {
        self.entries.remove(&FdKey {
            trace_id,
            process,
            fd,
        })
    }

    pub fn close_range(
        &mut self,
        trace_id: TraceId,
        process: ProcessIdentity,
        first_fd: i32,
        last_fd: i32,
    ) {
        let keys = self
            .entries
            .keys()
            .filter(|key| {
                key.trace_id == trace_id
                    && key.process == process
                    && key.fd >= first_fd
                    && key.fd <= last_fd
            })
            .copied()
            .collect::<Vec<_>>();
        for key in keys {
            self.entries.remove(&key);
        }
    }

    pub fn get(&self, key: FdKey) -> Option<FdState> {
        self.entries.get(&key).copied()
    }

    /// Channel id of a pipe/Unix-socket endpoint; the daemon recognizes
    /// orchestration channels (created by the trace root) across fork/dup.
    pub fn channel_of(&self, trace_id: TraceId, process: ProcessIdentity, fd: i32) -> Option<u64> {
        self.get(FdKey {
            trace_id,
            process,
            fd,
        })
        .map(|state| state.channel_id)
    }

    /// Whether one fd is tracked as a pipe or Unix-socket endpoint.
    pub fn is_channel_fd(&self, trace_id: TraceId, process: ProcessIdentity, fd: i32) -> bool {
        matches!(
            self.get(FdKey {
                trace_id,
                process,
                fd,
            })
            .map(|state| state.kind),
            Some(FdChannelKind::Pipe) | Some(FdChannelKind::UnixSocket)
        )
    }

    fn allocate_channel(&mut self) -> u64 {
        let id = self.next_channel_id;
        self.next_channel_id = self.next_channel_id.saturating_add(1);
        id
    }
}

#[cfg(test)]
mod tests {
    use model_core::ids::TraceId;
    use model_core::process::ProcessIdentity;

    use super::*;

    #[test]
    fn pipe_lineage_survives_dup_and_fork_but_not_cross_trace() {
        let trace = TraceId::new(1);
        let other = TraceId::new(2);
        let parent = ProcessIdentity::new(10);
        let child = ProcessIdentity::new(11);
        let mut lineage = FdLineage::new();
        lineage.pipe_pair(trace, parent, 3, 4);
        let original = lineage
            .get(FdKey {
                trace_id: trace,
                process: parent,
                fd: 3,
            })
            .unwrap();
        lineage.dup(trace, parent, 3, 7).unwrap();
        lineage.fork(trace, parent, child);
        assert_eq!(
            lineage.get(FdKey {
                trace_id: trace,
                process: child,
                fd: 7
            }),
            Some(original)
        );
        assert_eq!(
            lineage.get(FdKey {
                trace_id: other,
                process: child,
                fd: 3
            }),
            None
        );
        lineage.close_range(trace, parent, 6, 7);
        assert!(
            lineage
                .get(FdKey {
                    trace_id: trace,
                    process: parent,
                    fd: 7,
                })
                .is_none()
        );
        assert!(
            lineage
                .get(FdKey {
                    trace_id: trace,
                    process: child,
                    fd: 7,
                })
                .is_some()
        );
        lineage.close(trace, parent, 3);
        assert_eq!(
            lineage.get(FdKey {
                trace_id: trace,
                process: parent,
                fd: 3
            }),
            None
        );
    }

    #[test]
    fn socketpair_lineage_preserves_peer_identity_until_each_fd_is_closed() {
        let trace = TraceId::new(3);
        let process = ProcessIdentity::new(20);
        let mut lineage = FdLineage::new();
        lineage.socket_pair(trace, process, 5, 6);

        let first = lineage
            .get(FdKey {
                trace_id: trace,
                process,
                fd: 5,
            })
            .expect("first socket exists");
        let second = lineage
            .get(FdKey {
                trace_id: trace,
                process,
                fd: 6,
            })
            .expect("second socket exists");
        assert_eq!(first.kind, FdChannelKind::UnixSocket);
        assert_eq!(first.channel_id, second.channel_id);
        assert_eq!(first.peer_fd, Some(6));
        assert_eq!(second.peer_fd, Some(5));

        lineage
            .dup(trace, process, 5, 9)
            .expect("dup inherits state");
        assert_eq!(
            lineage
                .get(FdKey {
                    trace_id: trace,
                    process,
                    fd: 9,
                })
                .expect("duplicated socket exists")
                .channel_id,
            first.channel_id
        );
        lineage.close(trace, process, 5);
        assert!(
            lineage
                .get(FdKey {
                    trace_id: trace,
                    process,
                    fd: 9,
                })
                .is_some()
        );
        assert!(
            lineage
                .get(FdKey {
                    trace_id: TraceId::new(4),
                    process,
                    fd: 9,
                })
                .is_none()
        );
    }

    #[test]
    fn channel_queries_follow_dup_and_fork_and_exclude_files() {
        let trace = TraceId::new(7);
        let parent = ProcessIdentity::new(21);
        let child = ProcessIdentity::new(22);
        let mut lineage = FdLineage::new();
        lineage.pipe_pair(trace, parent, 3, 4);
        lineage.dup(trace, parent, 4, 9).unwrap();
        lineage.fork(trace, parent, child);
        let p = lineage.channel_of(trace, parent, 3).unwrap();
        assert_eq!(lineage.channel_of(trace, parent, 4), Some(p));
        assert_eq!(lineage.channel_of(trace, parent, 9), Some(p));
        assert_eq!(lineage.channel_of(trace, child, 3), Some(p));
        assert!(lineage.is_channel_fd(trace, child, 4));
        lineage.insert(
            FdKey {
                trace_id: trace,
                process: parent,
                fd: 7,
            },
            FdChannelKind::File,
        );
        assert!(!lineage.is_channel_fd(trace, parent, 7));
        // channel_of is unselective; callers gate on is_channel_fd first.
        assert!(lineage.channel_of(trace, parent, 7).is_some());
    }
}
