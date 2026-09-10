//! Trace-scoped cwd and file-descriptor path resolution.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use model_core::ids::TraceId;

const AT_FDCWD: i32 = -100;

#[derive(Clone, Copy, Debug)]
pub(crate) struct FileObservation<'a> {
    pub trace_id: TraceId,
    pub pid: u32,
    pub generation: u64,
    pub operation: &'a str,
    pub fd: u32,
    pub dirfd: Option<i32>,
    pub target_dirfd: Option<i32>,
    pub target_fd: Option<u32>,
    pub last_fd: Option<u32>,
    pub result: i32,
    pub raw_path: Option<&'a str>,
    pub raw_path2: Option<&'a str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FileResolution {
    pub path: Option<String>,
    pub target_path: Option<String>,
    pub source: &'static str,
    pub target_source: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct ProcessKey {
    trace_id: TraceId,
    pid: u32,
    generation: u64,
}

#[derive(Clone, Debug, Default)]
struct ProcessPaths {
    cwd: Option<PathBuf>,
    fds: BTreeMap<u32, PathBuf>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct PathState {
    processes: BTreeMap<ProcessKey, ProcessPaths>,
}

impl PathState {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn observe(&mut self, event: FileObservation<'_>) -> FileResolution {
        let key = ProcessKey {
            trace_id: event.trace_id,
            pid: event.pid,
            generation: event.generation,
        };
        self.ensure_process(key);
        let (path, source) = self.resolve(key, event.raw_path, event.dirfd, event.fd);
        let (target_path, target_source) =
            self.resolve(key, event.raw_path2, event.target_dirfd, event.fd);

        if event.result >= 0 {
            self.apply_success(key, &event, path.as_deref(), target_path.as_deref());
        }

        FileResolution {
            path,
            target_path,
            source,
            target_source,
        }
    }

    pub(crate) fn fork(
        &mut self,
        trace_id: TraceId,
        parent_pid: u32,
        parent_generation: u64,
        child_pid: u32,
        child_generation: u64,
    ) {
        let parent = ProcessKey {
            trace_id,
            pid: parent_pid,
            generation: parent_generation,
        };
        self.ensure_process(parent);
        if let Some(paths) = self.processes.get(&parent).cloned() {
            self.processes.insert(
                ProcessKey {
                    trace_id,
                    pid: child_pid,
                    generation: child_generation,
                },
                paths,
            );
        }
    }

    pub(crate) fn release(&mut self, trace_id: TraceId, pid: u32, generation: u64) {
        self.processes.remove(&ProcessKey {
            trace_id,
            pid,
            generation,
        });
    }

    pub(crate) fn release_pid(&mut self, trace_id: TraceId, pid: u32) {
        self.processes
            .retain(|key, _| key.trace_id != trace_id || key.pid != pid);
    }

    fn ensure_process(&mut self, key: ProcessKey) {
        self.processes.entry(key).or_insert_with(|| ProcessPaths {
            cwd: read_process_link(key.pid, "cwd"),
            fds: BTreeMap::new(),
        });
    }

    fn resolve(
        &mut self,
        key: ProcessKey,
        raw_path: Option<&str>,
        dirfd: Option<i32>,
        fd: u32,
    ) -> (Option<String>, &'static str) {
        if let Some(raw_path) = raw_path {
            let path = Path::new(raw_path);
            if path.is_absolute() {
                return (
                    Some(normalize(path).to_string_lossy().into_owned()),
                    "absolute",
                );
            }
            let base = match dirfd {
                None | Some(AT_FDCWD) => self
                    .processes
                    .get(&key)
                    .and_then(|state| state.cwd.clone())
                    .map(|path| (path, "cwd")),
                Some(dirfd) if dirfd >= 0 => {
                    self.fd_path(key, dirfd as u32).map(|path| (path, "dirfd"))
                }
                _ => None,
            };
            return match base {
                Some((base, source)) => (
                    Some(normalize(&base.join(path)).to_string_lossy().into_owned()),
                    source,
                ),
                None => (None, "unresolved_relative"),
            };
        }

        match self.fd_path(key, fd) {
            Some(path) => (Some(path.to_string_lossy().into_owned()), "fd"),
            None => (None, "unresolved_fd"),
        }
    }

    fn fd_path(&mut self, key: ProcessKey, fd: u32) -> Option<PathBuf> {
        if let Some(path) = self
            .processes
            .get(&key)
            .and_then(|state| state.fds.get(&fd))
            .cloned()
        {
            return Some(path);
        }
        let path = read_process_link(key.pid, &format!("fd/{fd}"))?;
        if !path.is_absolute() {
            return None;
        }
        self.processes
            .get_mut(&key)
            .expect("process state exists")
            .fds
            .insert(fd, path.clone());
        Some(path)
    }

    fn apply_success(
        &mut self,
        key: ProcessKey,
        event: &FileObservation<'_>,
        path: Option<&str>,
        target_path: Option<&str>,
    ) {
        let state = self.processes.get_mut(&key).expect("process state exists");
        match event.operation {
            "open" | "openat" | "openat2" | "creat" => {
                if let Some(path) = path {
                    state.fds.insert(event.fd, PathBuf::from(path));
                }
            }
            "close" => {
                state.fds.remove(&event.fd);
            }
            "close_range" => {
                let last = event.last_fd.unwrap_or(u32::MAX);
                state.fds.retain(|fd, _| *fd < event.fd || *fd > last);
            }
            "dup" | "dup2" | "dup3" | "fcntl" => {
                if let (Some(target), Some(source)) =
                    (event.target_fd, state.fds.get(&event.fd).cloned())
                {
                    state.fds.insert(target, source);
                }
            }
            "chdir" => {
                if let Some(path) = path {
                    state.cwd = Some(PathBuf::from(path));
                }
            }
            "fchdir" => {
                if let Some(path) = state.fds.get(&event.fd).cloned() {
                    state.cwd = Some(path);
                }
            }
            "renameat" | "renameat2" | "rename" => {
                if let (Some(source), Some(target)) = (path, target_path) {
                    rewrite_paths(state, Path::new(source), Path::new(target));
                }
            }
            _ => {}
        }
    }
}

fn read_process_link(pid: u32, relative: &str) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/{relative}")).ok()
}

fn rewrite_paths(state: &mut ProcessPaths, source: &Path, target: &Path) {
    if let Some(cwd) = state.cwd.clone()
        && let Ok(suffix) = cwd.strip_prefix(source)
    {
        state.cwd = Some(target.join(suffix));
    }
    for path in state.fds.values_mut() {
        if let Ok(suffix) = path.strip_prefix(source) {
            *path = target.join(suffix);
        }
    }
}

fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => normalized.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(value) => normalized.push(value),
            Component::Prefix(_) => unreachable!("Linux paths have no prefix component"),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation<'a>(operation: &'a str, raw_path: Option<&'a str>) -> FileObservation<'a> {
        FileObservation {
            trace_id: TraceId::new(1),
            pid: std::process::id(),
            generation: 9,
            operation,
            fd: 7,
            dirfd: Some(AT_FDCWD),
            target_dirfd: None,
            target_fd: None,
            last_fd: None,
            result: 0,
            raw_path,
            raw_path2: None,
        }
    }

    #[test]
    fn relative_paths_follow_cwd_and_chdir() {
        let mut state = PathState::new();
        let key = ProcessKey {
            trace_id: TraceId::new(1),
            pid: std::process::id(),
            generation: 9,
        };
        state.processes.insert(
            key,
            ProcessPaths {
                cwd: Some(PathBuf::from("/work/root")),
                fds: BTreeMap::new(),
            },
        );

        let resolved = state.observe(observation("openat", Some("a/../b.txt")));
        assert_eq!(resolved.path.as_deref(), Some("/work/root/b.txt"));
        assert_eq!(resolved.source, "cwd");

        state.observe(observation("chdir", Some("next")));
        let resolved = state.observe(observation("openat", Some("file")));
        assert_eq!(resolved.path.as_deref(), Some("/work/root/next/file"));
    }

    #[test]
    fn fork_keeps_paths_trace_and_generation_scoped() {
        let mut state = PathState::new();
        let mut open = observation("openat", Some("/tmp/value"));
        open.result = 7;
        state.observe(open);
        state.fork(TraceId::new(1), std::process::id(), 9, 81, 10);

        let child = ProcessKey {
            trace_id: TraceId::new(1),
            pid: 81,
            generation: 10,
        };
        assert_eq!(
            state.processes[&child].fds.get(&7),
            Some(&PathBuf::from("/tmp/value"))
        );
        assert!(!state.processes.contains_key(&ProcessKey {
            trace_id: TraceId::new(2),
            pid: 81,
            generation: 10,
        }));
    }
}
