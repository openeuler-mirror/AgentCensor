use crate::codec;
use crate::error::{ErrorCode, Result, CensorFsError};
use crate::ids::*;
use crate::model::*;
use crate::persist::Persistence;
use crate::store::VersionStore;
use crate::upper::UpperDirectory;
use parking_lot::{Condvar, Mutex, RwLock};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(u64::MAX as u128) as u64
}

#[derive(Debug, Default)]
struct PathLockState {
    held: HashSet<LogicalPath>,
}

#[derive(Debug, Default)]
struct PathLockTable {
    state: Mutex<PathLockState>,
    cv: Condvar,
}

#[derive(Debug)]
struct PathGuard {
    table: Arc<PathLockTable>,
    paths: Vec<LogicalPath>,
}

impl PathLockTable {
    fn lock(self: &Arc<Self>, mut paths: Vec<LogicalPath>) -> PathGuard {
        paths.sort();
        paths.dedup();
        let mut state = self.state.lock();
        while paths.iter().any(|path| {
            state.held.iter().any(|held| {
                path == held || path.starts_with_dir(held) || held.starts_with_dir(path)
            })
        }) {
            self.cv.wait(&mut state);
        }
        state.held.extend(paths.iter().cloned());
        drop(state);
        PathGuard {
            table: Arc::clone(self),
            paths,
        }
    }
}

impl Drop for PathGuard {
    fn drop(&mut self) {
        let mut state = self.table.state.lock();
        for path in &self.paths {
            state.held.remove(path);
        }
        self.table.cv.notify_all();
    }
}

#[derive(Debug)]
struct MutationGuard<'a> {
    runtime: &'a TicketRuntime,
}

impl Drop for MutationGuard<'_> {
    fn drop(&mut self) {
        self.runtime.inflight.fetch_sub(1, Ordering::SeqCst);
        self.runtime.freeze_cv.notify_all();
    }
}

#[derive(Debug)]
struct OpenHandle {
    view_id: ViewId,
    path: LogicalPath,
    data: Vec<u8>,
    writable: bool,
    linked: bool,
}

#[derive(Debug)]
pub struct TicketRuntime {
    pub meta: RwLock<TicketMeta>,
    pub overlay: RwLock<BTreeMap<LogicalPath, OverlayEntry>>,
    upper: UpperDirectory,
    delta_path: PathBuf,
    delta_seq: AtomicU64,
    inflight: AtomicU64,
    writable_handles: AtomicU64,
    freeze_wait: Mutex<()>,
    freeze_cv: Condvar,
    path_locks: Arc<PathLockTable>,
    handles: Mutex<HashMap<u64, OpenHandle>>,
    next_handle: AtomicU64,
}

impl TicketRuntime {
    pub fn new(meta: TicketMeta, ticket_dir: &Path) -> Result<Self> {
        let upper = UpperDirectory::open(ticket_dir.join("upper"))?;
        let delta_path = ticket_dir.join("delta.log");
        if !delta_path.exists() {
            File::create(&delta_path)?.sync_all()?;
        }
        Ok(Self {
            meta: RwLock::new(meta),
            overlay: RwLock::new(BTreeMap::new()),
            upper,
            delta_path,
            delta_seq: AtomicU64::new(0),
            inflight: AtomicU64::new(0),
            writable_handles: AtomicU64::new(0),
            freeze_wait: Mutex::new(()),
            freeze_cv: Condvar::new(),
            path_locks: Arc::new(PathLockTable::default()),
            handles: Mutex::new(HashMap::new()),
            next_handle: AtomicU64::new(1),
        })
    }

    pub fn upper(&self) -> &UpperDirectory {
        &self.upper
    }
    pub fn writable_handles(&self) -> u64 {
        self.writable_handles.load(Ordering::SeqCst)
    }
    pub fn inflight_mutations(&self) -> u64 {
        self.inflight.load(Ordering::SeqCst)
    }

    fn open_handle(
        &self,
        view_id: ViewId,
        path: LogicalPath,
        data: Vec<u8>,
        writable: bool,
    ) -> Result<u64> {
        let meta = self.meta.read();
        if writable && meta.state != TicketState::Open {
            return Err(CensorFsError::new(
                ErrorCode::StateChanged,
                format!("ticket is {:?}", meta.state),
            ));
        }
        let handle = self.next_handle.fetch_add(1, Ordering::SeqCst);
        self.handles.lock().insert(
            handle,
            OpenHandle {
                view_id,
                path,
                data,
                writable,
                linked: true,
            },
        );
        if writable {
            self.writable_handles.fetch_add(1, Ordering::SeqCst);
        }
        Ok(handle)
    }

    fn release_handle(&self, handle: u64) -> Result<()> {
        let value = self
            .handles
            .lock()
            .remove(&handle)
            .ok_or_else(|| CensorFsError::not_found("file handle", handle))?;
        if value.writable {
            self.writable_handles.fetch_sub(1, Ordering::SeqCst);
        }
        self.freeze_cv.notify_all();
        Ok(())
    }

    fn release_view_handles(&self, view_id: ViewId) {
        let mut handles = self.handles.lock();
        let ids: Vec<_> = handles
            .iter()
            .filter_map(|(id, handle)| (handle.view_id == view_id).then_some(*id))
            .collect();
        for id in ids {
            if handles.remove(&id).is_some_and(|handle| handle.writable) {
                self.writable_handles.fetch_sub(1, Ordering::SeqCst);
            }
        }
        self.freeze_cv.notify_all();
    }

    fn mark_unlinked(&self, path: &LogicalPath) {
        for handle in self.handles.lock().values_mut() {
            if &handle.path == path {
                handle.linked = false;
            }
        }
    }

    fn move_handles(&self, source: &LogicalPath, target: &LogicalPath) {
        for handle in self.handles.lock().values_mut() {
            if &handle.path == source || handle.path.starts_with_dir(source) {
                let suffix = &handle.path.as_bytes()[source.as_bytes().len()..];
                let mut bytes = target.as_bytes().to_vec();
                bytes.extend_from_slice(suffix);
                if let Ok(path) = LogicalPath::new(bytes) {
                    handle.path = path;
                }
            }
        }
    }

    fn replace_linked_data(&self, path: &LogicalPath, data: &[u8]) {
        for handle in self.handles.lock().values_mut() {
            if handle.linked && &handle.path == path {
                handle.data = data.to_vec();
            }
        }
    }

    fn mutation(&self) -> Result<MutationGuard<'_>> {
        let state = self.meta.read();
        if state.state != TicketState::Open {
            return Err(CensorFsError::new(
                ErrorCode::StateChanged,
                format!("ticket is {:?}", state.state),
            ));
        }
        self.inflight.fetch_add(1, Ordering::SeqCst);
        drop(state);
        Ok(MutationGuard { runtime: self })
    }

    fn append_delta(
        &self,
        op: DeltaOp,
        path: LogicalPath,
        new_path: Option<LogicalPath>,
        overlay: Option<OverlayEntry>,
    ) -> Result<()> {
        let record = DeltaRecord {
            seq: self.delta_seq.fetch_add(1, Ordering::SeqCst) + 1,
            op,
            path,
            new_path,
            overlay,
        };
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.delta_path)?;
        codec::write_journal_frame(&mut file, &record)?;
        Ok(())
    }

    pub fn sync_private_state(&self) -> Result<()> {
        let overlay = self.overlay.read();
        self.upper
            .sync_names(overlay.values().filter_map(|entry| match entry {
                OverlayEntry::File { storage_name, .. } => Some(storage_name.as_str()),
                _ => None,
            }))?;
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.delta_path)?
            .sync_all()?;
        Ok(())
    }

    pub fn freeze(&self, timeout: Duration) -> Result<BTreeMap<LogicalPath, OverlayEntry>> {
        {
            let mut meta = self.meta.write();
            match meta.state {
                TicketState::Open => meta.state = TicketState::Freezing,
                TicketState::Prepared => return Ok(self.overlay.read().clone()),
                _ => {
                    return Err(CensorFsError::new(
                        ErrorCode::StateChanged,
                        format!("cannot prepare ticket in {:?}", meta.state),
                    ))
                }
            }
        }
        let deadline = Instant::now() + timeout;
        let mut wait = self.freeze_wait.lock();
        while self.inflight_mutations() != 0 || self.writable_handles() != 0 {
            let now = Instant::now();
            if now >= deadline {
                self.meta.write().state = TicketState::Open;
                return Err(CensorFsError::new(
                    ErrorCode::BusyOpenWriters,
                    "ticket still has active mutations or writable handles",
                ));
            }
            self.freeze_cv.wait_for(&mut wait, deadline - now);
        }
        drop(wait);
        self.sync_private_state()?;
        Ok(self.overlay.read().clone())
    }
}

#[derive(Debug, Clone)]
struct ViewContext {
    handle: ViewHandle,
    manifest: Arc<Manifest>,
    ticket: Option<Arc<TicketRuntime>>,
}

#[derive(Debug)]
pub struct ViewEngine {
    store: VersionStore,
    persist: Persistence,
    views: RwLock<HashMap<ViewId, ViewContext>>,
}

impl ViewEngine {
    pub fn new(store: VersionStore, persist: Persistence) -> Self {
        Self {
            store,
            persist,
            views: RwLock::new(HashMap::new()),
        }
    }

    pub fn register_view(
        &self,
        handle: ViewHandle,
        ticket: Option<Arc<TicketRuntime>>,
    ) -> Result<()> {
        let (_, manifest) = self.store.load_generation(handle.generation_id)?;
        if handle.can_write && ticket.is_none() {
            return Err(CensorFsError::bad_request("writable view requires a ticket"));
        }
        self.views.write().insert(
            handle.view_id,
            ViewContext {
                handle,
                manifest: Arc::new(manifest),
                ticket,
            },
        );
        Ok(())
    }

    pub fn view(&self, id: ViewId) -> Result<ViewHandle> {
        self.views
            .read()
            .get(&id)
            .map(|v| v.handle.clone())
            .ok_or_else(|| CensorFsError::not_found("view", id))
    }

    pub fn revoke(&self, id: ViewId) -> Result<()> {
        let mut views = self.views.write();
        let view = views
            .get_mut(&id)
            .ok_or_else(|| CensorFsError::not_found("view", id))?;
        view.handle.state = ViewState::Revoked;
        Ok(())
    }

    pub fn revoke_ticket_views(&self, ticket_id: TicketId) -> usize {
        let mut views = self.views.write();
        let mut revoked = 0;
        for view in views.values_mut() {
            if view.handle.ticket_id == Some(ticket_id)
                && matches!(view.handle.state, ViewState::Created | ViewState::Mounted)
            {
                view.handle.state = ViewState::Revoked;
                revoked += 1;
            }
        }
        revoked
    }

    pub fn mark_mounted(&self, id: ViewId, caller_uid: u32) -> Result<ViewHandle> {
        let mut views = self.views.write();
        let view = views
            .get_mut(&id)
            .ok_or_else(|| CensorFsError::not_found("view", id))?;
        if caller_uid != 0 && caller_uid != view.handle.owner_uid {
            return Err(CensorFsError::new(
                ErrorCode::AccessDenied,
                "view belongs to another uid",
            ));
        }
        if view.handle.state != ViewState::Created {
            return Err(CensorFsError::new(
                ErrorCode::StateChanged,
                "view is not in CREATED state",
            ));
        }
        view.handle.state = ViewState::Mounted;
        Ok(view.handle.clone())
    }

    pub fn close(&self, id: ViewId) -> Result<()> {
        let mut views = self.views.write();
        let mut view = views
            .remove(&id)
            .ok_or_else(|| CensorFsError::not_found("view", id))?;
        if let Some(ticket) = &view.ticket {
            ticket.release_view_handles(id);
        }
        view.handle.state = ViewState::Closed;
        Ok(())
    }

    fn context(&self, id: ViewId) -> Result<ViewContext> {
        let view = self
            .views
            .read()
            .get(&id)
            .cloned()
            .ok_or_else(|| CensorFsError::not_found("view", id))?;
        if matches!(view.handle.state, ViewState::Revoked | ViewState::Closed) {
            return Err(CensorFsError::new(
                ErrorCode::StateChanged,
                "view is no longer active",
            ));
        }
        Ok(view)
    }

    fn hidden_by_deleted_ancestor(
        overlay: &BTreeMap<LogicalPath, OverlayEntry>,
        path: &LogicalPath,
    ) -> bool {
        let mut current = path.parent();
        while let Some(parent) = current {
            if matches!(overlay.get(&parent), Some(OverlayEntry::Deleted)) {
                return true;
            }
            current = parent.parent();
        }
        false
    }

    fn lookup_in(view: &ViewContext, path: &LogicalPath) -> Option<ManifestEntry> {
        if let Some(ticket) = &view.ticket {
            let overlay = ticket.overlay.read();
            if Self::hidden_by_deleted_ancestor(&overlay, path) {
                return None;
            }
            if let Some(entry) = overlay.get(path) {
                return match entry {
                    OverlayEntry::Deleted => None,
                    OverlayEntry::Directory {
                        mode,
                        atime_ns,
                        mtime_ns,
                    } => Some(ManifestEntry {
                        path: path.clone(),
                        kind: EntryKind::Directory,
                        mode: *mode,
                        size: 0,
                        atime_ns: *atime_ns,
                        mtime_ns: *mtime_ns,
                        object_id: None,
                        content_digest: None,
                    }),
                    OverlayEntry::File {
                        mode,
                        atime_ns,
                        mtime_ns,
                        size,
                        ..
                    } => Some(ManifestEntry {
                        path: path.clone(),
                        kind: EntryKind::File,
                        mode: *mode,
                        size: *size,
                        atime_ns: *atime_ns,
                        mtime_ns: *mtime_ns,
                        object_id: None,
                        content_digest: None,
                    }),
                };
            }
        }
        view.manifest.entries.get(path).cloned()
    }

    pub fn lookup(&self, view_id: ViewId, path: &LogicalPath) -> Result<ManifestEntry> {
        let view = self.context(view_id)?;
        Self::lookup_in(&view, path).ok_or_else(|| CensorFsError::not_found("path", path))
    }

    pub fn read_file(&self, view_id: ViewId, path: &LogicalPath) -> Result<Vec<u8>> {
        let view = self.context(view_id)?;
        let entry =
            Self::lookup_in(&view, path).ok_or_else(|| CensorFsError::not_found("path", path))?;
        if entry.kind != EntryKind::File {
            return Err(CensorFsError::bad_request("path is not a regular file"));
        }
        if let Some(ticket) = &view.ticket {
            if let Some(OverlayEntry::File { storage_name, .. }) = ticket.overlay.read().get(path) {
                return ticket.upper.read(storage_name);
            }
        }
        self.store
            .read_object(entry.object_id.unwrap(), entry.content_digest.unwrap())
    }

    pub fn open_file(&self, view_id: ViewId, path: &LogicalPath, writable: bool) -> Result<u64> {
        let view = self.context(view_id)?;
        if writable && !view.handle.can_write {
            return Err(CensorFsError::new(ErrorCode::ReadOnly, "view is read-only"));
        }
        let data = self.read_file(view_id, path)?;
        match view.ticket {
            Some(ticket) => ticket.open_handle(view_id, path.clone(), data, writable),
            None => Ok(0),
        }
    }

    pub fn read_handle(
        &self,
        view_id: ViewId,
        handle: u64,
        path: &LogicalPath,
        offset: u64,
        size: u32,
    ) -> Result<Vec<u8>> {
        if handle == 0 {
            let data = self.read_file(view_id, path)?;
            let start = usize::try_from(offset)
                .unwrap_or(usize::MAX)
                .min(data.len());
            return Ok(data[start..start.saturating_add(size as usize).min(data.len())].to_vec());
        }
        let view = self.context(view_id)?;
        let ticket = view
            .ticket
            .ok_or_else(|| CensorFsError::not_found("file handle", handle))?;
        let handles = ticket.handles.lock();
        let file = handles
            .get(&handle)
            .filter(|value| value.view_id == view_id)
            .ok_or_else(|| CensorFsError::not_found("file handle", handle))?;
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(file.data.len());
        Ok(file.data[start..start.saturating_add(size as usize).min(file.data.len())].to_vec())
    }

    pub fn write_handle(
        &self,
        view_id: ViewId,
        handle: u64,
        offset: u64,
        data: &[u8],
    ) -> Result<u32> {
        let view = self.context(view_id)?;
        let ticket = view
            .ticket
            .as_ref()
            .ok_or_else(|| CensorFsError::new(ErrorCode::ReadOnly, "view is read-only"))?;
        let path = {
            let handles = ticket.handles.lock();
            handles
                .get(&handle)
                .filter(|value| value.view_id == view_id)
                .map(|value| value.path.clone())
                .ok_or_else(|| CensorFsError::not_found("file handle", handle))?
        };
        let _mutation = ticket.mutation()?;
        let _paths = ticket.path_locks.lock(vec![path.clone()]);
        let (contents, linked, mode, atime_ns) = {
            let mut handles = ticket.handles.lock();
            let file = handles
                .get_mut(&handle)
                .filter(|value| value.view_id == view_id)
                .ok_or_else(|| CensorFsError::not_found("file handle", handle))?;
            if !file.writable {
                return Err(CensorFsError::new(
                    ErrorCode::AccessDenied,
                    "file handle is read-only",
                ));
            }
            let start = usize::try_from(offset)
                .map_err(|_| CensorFsError::bad_request("write offset is too large"))?;
            let end = start
                .checked_add(data.len())
                .ok_or_else(|| CensorFsError::bad_request("write range overflows"))?;
            if file.data.len() < end {
                file.data.resize(end, 0);
            }
            file.data[start..end].copy_from_slice(data);
            let (mode, atime_ns) = Self::lookup_in(&view, &file.path)
                .map(|entry| (entry.mode, entry.atime_ns))
                .unwrap_or((0o644, now_ns()));
            (file.data.clone(), file.linked, mode, atime_ns)
        };
        if linked {
            ticket.replace_linked_data(&path, &contents);
        }
        if linked {
            let (storage_name, size) = Self::new_upper_file(ticket, &contents)?;
            let entry = OverlayEntry::File {
                storage_name,
                mode,
                atime_ns,
                mtime_ns: now_ns(),
                size,
            };
            ticket.overlay.write().insert(path.clone(), entry.clone());
            ticket.append_delta(DeltaOp::Upsert, path, None, Some(entry))?;
        }
        Ok(data.len() as u32)
    }

    pub fn truncate_handle(&self, view_id: ViewId, handle: u64, size: u64) -> Result<()> {
        let view = self.context(view_id)?;
        let ticket = view
            .ticket
            .as_ref()
            .ok_or_else(|| CensorFsError::new(ErrorCode::ReadOnly, "view is read-only"))?;
        let path = {
            let handles = ticket.handles.lock();
            handles
                .get(&handle)
                .filter(|value| value.view_id == view_id)
                .map(|value| value.path.clone())
                .ok_or_else(|| CensorFsError::not_found("file handle", handle))?
        };
        let size = usize::try_from(size)
            .map_err(|_| CensorFsError::bad_request("file size exceeds address space"))?;
        let _mutation = ticket.mutation()?;
        let _paths = ticket.path_locks.lock(vec![path.clone()]);
        let (contents, linked, mode, atime_ns) = {
            let mut handles = ticket.handles.lock();
            let file = handles
                .get_mut(&handle)
                .filter(|value| value.view_id == view_id)
                .ok_or_else(|| CensorFsError::not_found("file handle", handle))?;
            if !file.writable {
                return Err(CensorFsError::new(
                    ErrorCode::AccessDenied,
                    "file handle is read-only",
                ));
            }
            file.data.resize(size, 0);
            let (mode, atime_ns) = Self::lookup_in(&view, &file.path)
                .map(|entry| (entry.mode, entry.atime_ns))
                .unwrap_or((0o644, now_ns()));
            (file.data.clone(), file.linked, mode, atime_ns)
        };
        if linked {
            ticket.replace_linked_data(&path, &contents);
        }
        if linked {
            let (storage_name, length) = Self::new_upper_file(ticket, &contents)?;
            let entry = OverlayEntry::File {
                storage_name,
                mode,
                atime_ns,
                mtime_ns: now_ns(),
                size: length,
            };
            ticket.overlay.write().insert(path.clone(), entry.clone());
            ticket.append_delta(DeltaOp::Upsert, path, None, Some(entry))?;
        }
        Ok(())
    }

    pub fn release_file(&self, view_id: ViewId, handle: u64) -> Result<()> {
        if handle == 0 {
            return Ok(());
        }
        let view = self
            .views
            .read()
            .get(&view_id)
            .cloned()
            .ok_or_else(|| CensorFsError::not_found("view", view_id))?;
        let ticket = view
            .ticket
            .ok_or_else(|| CensorFsError::not_found("file handle", handle))?;
        ticket.release_handle(handle)
    }

    fn require_parent(view: &ViewContext, path: &LogicalPath) -> Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| CensorFsError::bad_request("cannot mutate the view root"))?;
        match Self::lookup_in(view, &parent) {
            Some(entry) if entry.kind == EntryKind::Directory => Ok(()),
            _ => Err(CensorFsError::not_found("parent directory", parent)),
        }
    }

    fn new_upper_file(ticket: &TicketRuntime, data: &[u8]) -> Result<(String, u64)> {
        ticket.upper.write_new(data)
    }

    pub fn create_file(
        &self,
        view_id: ViewId,
        path: LogicalPath,
        data: &[u8],
        mode: u32,
        exclusive: bool,
    ) -> Result<()> {
        let view = self.context(view_id)?;
        if !view.handle.can_write {
            return Err(CensorFsError::new(ErrorCode::ReadOnly, "view is read-only"));
        }
        let ticket = view.ticket.as_ref().unwrap();
        let _mutation = ticket.mutation()?;
        let _paths = ticket.path_locks.lock(vec![path.clone()]);
        Self::require_parent(&view, &path)?;
        if exclusive && Self::lookup_in(&view, &path).is_some() {
            return Err(CensorFsError::new(
                ErrorCode::AlreadyExistsDifferent,
                "path already exists",
            ));
        }
        if matches!(Self::lookup_in(&view, &path), Some(e) if e.kind == EntryKind::Directory) {
            return Err(CensorFsError::bad_request(
                "cannot replace a directory with a file",
            ));
        }
        let atime_ns = Self::lookup_in(&view, &path)
            .map(|entry| entry.atime_ns)
            .unwrap_or_else(now_ns);
        let (storage_name, size) = Self::new_upper_file(ticket, data)?;
        let entry = OverlayEntry::File {
            storage_name,
            mode: mode & 0o777,
            atime_ns,
            mtime_ns: now_ns(),
            size,
        };
        ticket.overlay.write().insert(path.clone(), entry.clone());
        ticket.replace_linked_data(&path, data);
        ticket.append_delta(DeltaOp::Upsert, path, None, Some(entry))
    }

    pub fn write_file(&self, view_id: ViewId, path: LogicalPath, data: &[u8]) -> Result<()> {
        let existing = self.lookup(view_id, &path);
        match existing {
            Ok(entry) if entry.kind == EntryKind::File => {
                self.create_file(view_id, path, data, entry.mode, false)
            }
            Ok(_) => Err(CensorFsError::bad_request("path is not a regular file")),
            Err(error) if error.code == ErrorCode::NotFound => {
                self.create_file(view_id, path, data, 0o644, true)
            }
            Err(error) => Err(error),
        }
    }

    pub fn truncate(&self, view_id: ViewId, path: LogicalPath, size: u64) -> Result<()> {
        let mut data = self.read_file(view_id, &path)?;
        let size = usize::try_from(size)
            .map_err(|_| CensorFsError::bad_request("file size exceeds address space"))?;
        data.resize(size, 0);
        self.write_file(view_id, path, &data)
    }

    pub fn mkdir(&self, view_id: ViewId, path: LogicalPath, mode: u32) -> Result<()> {
        let view = self.context(view_id)?;
        if !view.handle.can_write {
            return Err(CensorFsError::new(ErrorCode::ReadOnly, "view is read-only"));
        }
        let ticket = view.ticket.as_ref().unwrap();
        let _mutation = ticket.mutation()?;
        let _paths = ticket.path_locks.lock(vec![path.clone()]);
        Self::require_parent(&view, &path)?;
        if Self::lookup_in(&view, &path).is_some() {
            return Err(CensorFsError::new(
                ErrorCode::AlreadyExistsDifferent,
                "path already exists",
            ));
        }
        let entry = OverlayEntry::Directory {
            mode: mode & 0o777,
            atime_ns: now_ns(),
            mtime_ns: now_ns(),
        };
        ticket.overlay.write().insert(path.clone(), entry.clone());
        ticket.append_delta(DeltaOp::Mkdir, path, None, Some(entry))
    }

    pub fn readdir(&self, view_id: ViewId, directory: &LogicalPath) -> Result<Vec<ManifestEntry>> {
        let view = self.context(view_id)?;
        let dir = Self::lookup_in(&view, directory)
            .ok_or_else(|| CensorFsError::not_found("directory", directory))?;
        if dir.kind != EntryKind::Directory {
            return Err(CensorFsError::bad_request("path is not a directory"));
        }
        let mut paths = BTreeSet::new();
        for path in view.manifest.entries.keys() {
            if path.parent().as_ref() == Some(directory) {
                paths.insert(path.clone());
            }
        }
        if let Some(ticket) = &view.ticket {
            for path in ticket.overlay.read().keys() {
                if path.parent().as_ref() == Some(directory) {
                    paths.insert(path.clone());
                }
            }
        }
        Ok(paths
            .into_iter()
            .filter_map(|p| Self::lookup_in(&view, &p))
            .collect())
    }

    pub fn unlink(&self, view_id: ViewId, path: LogicalPath) -> Result<()> {
        let view = self.context(view_id)?;
        if !view.handle.can_write {
            return Err(CensorFsError::new(ErrorCode::ReadOnly, "view is read-only"));
        }
        let ticket = view.ticket.as_ref().unwrap();
        let _mutation = ticket.mutation()?;
        let _paths = ticket.path_locks.lock(vec![path.clone()]);
        let existing =
            Self::lookup_in(&view, &path).ok_or_else(|| CensorFsError::not_found("path", &path))?;
        if existing.kind != EntryKind::File {
            return Err(CensorFsError::bad_request("unlink requires a regular file"));
        }
        ticket
            .overlay
            .write()
            .insert(path.clone(), OverlayEntry::Deleted);
        ticket.mark_unlinked(&path);
        ticket.append_delta(DeltaOp::Delete, path, None, Some(OverlayEntry::Deleted))
    }

    pub fn rmdir(&self, view_id: ViewId, path: LogicalPath) -> Result<()> {
        let view = self.context(view_id)?;
        if !view.handle.can_write {
            return Err(CensorFsError::new(ErrorCode::ReadOnly, "view is read-only"));
        }
        if path.is_root() {
            return Err(CensorFsError::bad_request("cannot remove the view root"));
        }
        let ticket = view.ticket.as_ref().unwrap();
        let _mutation = ticket.mutation()?;
        let _paths = ticket.path_locks.lock(vec![path.clone()]);
        let existing =
            Self::lookup_in(&view, &path).ok_or_else(|| CensorFsError::not_found("path", &path))?;
        if existing.kind != EntryKind::Directory {
            return Err(CensorFsError::bad_request("rmdir requires a directory"));
        }
        if !self.readdir(view_id, &path)?.is_empty() {
            return Err(CensorFsError::new(
                ErrorCode::DirectoryNotEmpty,
                "directory is not empty",
            ));
        }
        ticket
            .overlay
            .write()
            .insert(path.clone(), OverlayEntry::Deleted);
        ticket.append_delta(DeltaOp::Rmdir, path, None, Some(OverlayEntry::Deleted))
    }

    fn merged_tree(view: &ViewContext) -> BTreeMap<LogicalPath, ManifestEntry> {
        let mut result = view.manifest.entries.clone();
        if let Some(ticket) = &view.ticket {
            for (path, entry) in ticket.overlay.read().iter() {
                match entry {
                    OverlayEntry::Deleted => {
                        result.retain(|p, _| p != path && !p.starts_with_dir(path));
                    }
                    OverlayEntry::Directory {
                        mode,
                        atime_ns,
                        mtime_ns,
                    } => {
                        result.insert(
                            path.clone(),
                            ManifestEntry {
                                path: path.clone(),
                                kind: EntryKind::Directory,
                                mode: *mode,
                                size: 0,
                                atime_ns: *atime_ns,
                                mtime_ns: *mtime_ns,
                                object_id: None,
                                content_digest: None,
                            },
                        );
                    }
                    OverlayEntry::File {
                        mode,
                        atime_ns,
                        mtime_ns,
                        size,
                        ..
                    } => {
                        result.insert(
                            path.clone(),
                            ManifestEntry {
                                path: path.clone(),
                                kind: EntryKind::File,
                                mode: *mode,
                                size: *size,
                                atime_ns: *atime_ns,
                                mtime_ns: *mtime_ns,
                                object_id: None,
                                content_digest: None,
                            },
                        );
                    }
                }
            }
        }
        result
    }

    pub fn rename(&self, view_id: ViewId, source: LogicalPath, target: LogicalPath) -> Result<()> {
        let view = self.context(view_id)?;
        if !view.handle.can_write {
            return Err(CensorFsError::new(ErrorCode::ReadOnly, "view is read-only"));
        }
        if source == target {
            return Ok(());
        }
        if target.starts_with_dir(&source) {
            return Err(CensorFsError::bad_request(
                "cannot move a directory inside itself",
            ));
        }
        let ticket = view.ticket.as_ref().unwrap();
        let _mutation = ticket.mutation()?;
        let _paths = ticket.path_locks.lock(vec![source.clone(), target.clone()]);
        Self::require_parent(&view, &target)?;
        let tree = Self::merged_tree(&view);
        let source_entry = tree
            .get(&source)
            .cloned()
            .ok_or_else(|| CensorFsError::not_found("path", &source))?;
        if let Some(target_entry) = tree.get(&target) {
            if source_entry.kind != target_entry.kind {
                return Err(CensorFsError::bad_request(
                    "rename cannot replace a path of a different type",
                ));
            }
            if target_entry.kind == EntryKind::Directory
                && tree.keys().any(|path| path.starts_with_dir(&target))
            {
                return Err(CensorFsError::new(
                    ErrorCode::StateChanged,
                    "rename target directory is not empty",
                ));
            }
        }
        let selected: Vec<_> = tree
            .iter()
            .filter(|(p, _)| *p == &source || p.starts_with_dir(&source))
            .map(|(p, e)| (p.clone(), e.clone()))
            .collect();
        let source_prefix = source.as_bytes().to_vec();
        let mut changes = Vec::new();
        for (old, entry) in selected {
            let suffix = &old.as_bytes()[source_prefix.len()..];
            let mut new_bytes = target.as_bytes().to_vec();
            new_bytes.extend_from_slice(suffix);
            let new_path = LogicalPath::new(new_bytes)?;
            let overlay_entry = match entry.kind {
                EntryKind::Directory => OverlayEntry::Directory {
                    mode: entry.mode,
                    atime_ns: entry.atime_ns,
                    mtime_ns: entry.mtime_ns,
                },
                EntryKind::File => {
                    if let Some(OverlayEntry::File {
                        storage_name,
                        mode,
                        atime_ns,
                        mtime_ns,
                        size,
                    }) = ticket.overlay.read().get(&old).cloned()
                    {
                        OverlayEntry::File {
                            storage_name,
                            mode,
                            atime_ns,
                            mtime_ns,
                            size,
                        }
                    } else {
                        let data = self
                            .store
                            .read_object(entry.object_id.unwrap(), entry.content_digest.unwrap())?;
                        let (storage_name, size) = Self::new_upper_file(ticket, &data)?;
                        OverlayEntry::File {
                            storage_name,
                            mode: entry.mode,
                            atime_ns: entry.atime_ns,
                            mtime_ns: entry.mtime_ns,
                            size,
                        }
                    }
                }
            };
            changes.push((new_path, overlay_entry));
        }
        {
            let mut overlay = ticket.overlay.write();
            overlay.insert(source.clone(), OverlayEntry::Deleted);
            for (path, entry) in &changes {
                overlay.insert(path.clone(), entry.clone());
            }
        }
        ticket.mark_unlinked(&target);
        ticket.move_handles(&source, &target);
        ticket.append_delta(DeltaOp::Rename, source, Some(target), None)
    }

    /// Create a hard link `target` -> `source`.
    ///
    /// CensorFS has no cross-path inode sharing: every overlay entry owns a
    /// private upper-layer object. So a hard link is modeled as a copy-up of
    /// the source content into a fresh object at the target path. This matches
    /// the only real caller (atomic no-replace publish: `link(temp, final)`
    /// then unlink temp), which never relies on shared-inode semantics — it
    /// only needs the publish to succeed and to fail with EEXIST if `target`
    /// already exists. Hard-linking a directory is rejected (EPERM), as on
    /// Linux.
    pub fn link(
        &self,
        view_id: ViewId,
        source: LogicalPath,
        target: LogicalPath,
    ) -> Result<()> {
        let view = self.context(view_id)?;
        if !view.handle.can_write {
            return Err(CensorFsError::new(ErrorCode::ReadOnly, "view is read-only"));
        }
        if source == target {
            return Err(CensorFsError::new(
                ErrorCode::AlreadyExistsDifferent,
                "link source and target are the same",
            ));
        }
        let ticket = view.ticket.as_ref().unwrap();
        let _mutation = ticket.mutation()?;
        let _paths = ticket.path_locks.lock(vec![source.clone(), target.clone()]);
        Self::require_parent(&view, &target)?;
        let source_entry = Self::lookup_in(&view, &source)
            .ok_or_else(|| CensorFsError::not_found("path", &source))?;
        if source_entry.kind != EntryKind::File {
            return Err(CensorFsError::new(
                ErrorCode::Unsupported,
                "hard links are only supported for regular files",
            ));
        }
        if Self::lookup_in(&view, &target).is_some() {
            return Err(CensorFsError::new(
                ErrorCode::AlreadyExistsDifferent,
                "target path already exists",
            ));
        }
        // Copy-up the source content into a new upper object at the target.
        let data = self.read_file(view_id, &source)?;
        let (storage_name, size) = Self::new_upper_file(ticket, &data)?;
        let entry = OverlayEntry::File {
            storage_name,
            mode: source_entry.mode,
            atime_ns: now_ns(),
            mtime_ns: source_entry.mtime_ns,
            size,
        };
        ticket.overlay.write().insert(target.clone(), entry.clone());
        ticket.append_delta(DeltaOp::Upsert, target, None, Some(entry))
    }

    pub fn chmod(&self, view_id: ViewId, path: LogicalPath, mode: u32) -> Result<()> {
        let view = self.context(view_id)?;
        if !view.handle.can_write {
            return Err(CensorFsError::new(ErrorCode::ReadOnly, "view is read-only"));
        }
        let ticket = view.ticket.as_ref().unwrap();
        let _mutation = ticket.mutation()?;
        let _paths = ticket.path_locks.lock(vec![path.clone()]);
        let entry =
            Self::lookup_in(&view, &path).ok_or_else(|| CensorFsError::not_found("path", &path))?;
        let updated = match ticket.overlay.read().get(&path).cloned() {
            Some(OverlayEntry::File {
                storage_name,
                atime_ns,
                mtime_ns,
                size,
                ..
            }) => OverlayEntry::File {
                storage_name,
                mode: mode & 0o777,
                atime_ns,
                mtime_ns,
                size,
            },
            Some(OverlayEntry::Directory {
                atime_ns, mtime_ns, ..
            }) => OverlayEntry::Directory {
                mode: mode & 0o777,
                atime_ns,
                mtime_ns,
            },
            _ if entry.kind == EntryKind::Directory => OverlayEntry::Directory {
                mode: mode & 0o777,
                atime_ns: entry.atime_ns,
                mtime_ns: entry.mtime_ns,
            },
            _ => {
                let data = self.read_file(view_id, &path)?;
                let (storage_name, size) = Self::new_upper_file(ticket, &data)?;
                OverlayEntry::File {
                    storage_name,
                    mode: mode & 0o777,
                    atime_ns: entry.atime_ns,
                    mtime_ns: entry.mtime_ns,
                    size,
                }
            }
        };
        ticket.overlay.write().insert(path.clone(), updated.clone());
        ticket.append_delta(DeltaOp::Metadata, path, None, Some(updated))
    }

    pub fn utimens(
        &self,
        view_id: ViewId,
        path: LogicalPath,
        atime_ns: u64,
        mtime_ns: u64,
    ) -> Result<()> {
        let view = self.context(view_id)?;
        if !view.handle.can_write {
            return Err(CensorFsError::new(ErrorCode::ReadOnly, "view is read-only"));
        }
        let ticket = view.ticket.as_ref().unwrap();
        let _mutation = ticket.mutation()?;
        let _paths = ticket.path_locks.lock(vec![path.clone()]);
        let entry =
            Self::lookup_in(&view, &path).ok_or_else(|| CensorFsError::not_found("path", &path))?;
        let updated = match ticket.overlay.read().get(&path).cloned() {
            Some(OverlayEntry::File {
                storage_name,
                mode,
                size,
                ..
            }) => OverlayEntry::File {
                storage_name,
                mode,
                atime_ns,
                mtime_ns,
                size,
            },
            Some(OverlayEntry::Directory { mode, .. }) => OverlayEntry::Directory {
                mode,
                atime_ns,
                mtime_ns,
            },
            _ if entry.kind == EntryKind::Directory => OverlayEntry::Directory {
                mode: entry.mode,
                atime_ns,
                mtime_ns,
            },
            _ => {
                let data = self.read_file(view_id, &path)?;
                let (storage_name, size) = Self::new_upper_file(ticket, &data)?;
                OverlayEntry::File {
                    storage_name,
                    mode: entry.mode,
                    atime_ns,
                    mtime_ns,
                    size,
                }
            }
        };
        ticket.overlay.write().insert(path.clone(), updated.clone());
        ticket.append_delta(DeltaOp::Metadata, path, None, Some(updated))
    }

    pub fn fsync(&self, view_id: ViewId) -> Result<()> {
        let view = self.context(view_id)?;
        if let Some(ticket) = view.ticket {
            ticket.sync_private_state()?;
        }
        Ok(())
    }

    pub fn persist_ticket_meta(&self, ticket: &TicketRuntime) -> Result<()> {
        let meta = ticket.meta.read().clone();
        self.persist.write_record(
            &self.persist.ticket_meta_path(meta.ticket_id),
            crate::codec::RecordKind::Ticket,
            &meta,
            false,
        )
    }
}
