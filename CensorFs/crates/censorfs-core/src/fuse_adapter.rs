//! Linux FUSE adapter. The stateful filesystem rules live in `ViewEngine`; this module only
//! translates kernel requests and deliberately opts writable ticket views out of page caching.

use crate::error::{ErrorCode, CensorFsError};
use crate::ids::ViewId;
use crate::model::{EntryKind, LogicalPath, ManifestEntry};
use crate::viewfs::ViewEngine;
use fuser::{
    FileAttr, FileType, Filesystem, KernelConfig, ReplyAttr, ReplyCreate, ReplyData,
    ReplyDirectory, ReplyEmpty, ReplyEntry, ReplyOpen, ReplyWrite, Request, TimeOrNow,
};
use parking_lot::{Mutex, RwLock};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const ROOT_INO: u64 = 1;
const ZERO_TTL: Duration = Duration::from_secs(0);
const READ_TTL: Duration = Duration::from_secs(60);

#[derive(Debug)]
pub struct FuseViewAdapter {
    engine: Arc<ViewEngine>,
    view_id: ViewId,
    writable: bool,
    inode_to_path: RwLock<HashMap<u64, LogicalPath>>,
    path_to_inode: RwLock<HashMap<LogicalPath, u64>>,
    directories: Mutex<HashMap<u64, Vec<(u64, FileType, String)>>>,
    next_directory: AtomicU64,
}

impl FuseViewAdapter {
    pub fn new(engine: Arc<ViewEngine>, view_id: ViewId) -> crate::Result<Self> {
        let writable = engine.view(view_id)?.can_write;
        let root = LogicalPath::root();
        Ok(Self {
            engine,
            view_id,
            writable,
            inode_to_path: RwLock::new(HashMap::from([(ROOT_INO, root.clone())])),
            path_to_inode: RwLock::new(HashMap::from([(root, ROOT_INO)])),
            directories: Mutex::new(HashMap::new()),
            next_directory: AtomicU64::new(1),
        })
    }

    fn ttl(&self) -> Duration {
        if self.writable {
            ZERO_TTL
        } else {
            READ_TTL
        }
    }

    fn inode(&self, path: &LogicalPath) -> u64 {
        if let Some(value) = self.path_to_inode.read().get(path) {
            return *value;
        }
        let mut digest = blake3::Hasher::new();
        digest.update(self.view_id.0.as_bytes());
        digest.update(path.as_bytes());
        let mut inode = u64::from_le_bytes(digest.finalize().as_bytes()[..8].try_into().unwrap());
        inode = inode.max(2);
        let mut by_inode = self.inode_to_path.write();
        while by_inode
            .get(&inode)
            .is_some_and(|existing| existing != path)
        {
            inode = inode.wrapping_add(1).max(2);
        }
        by_inode.insert(inode, path.clone());
        self.path_to_inode.write().insert(path.clone(), inode);
        inode
    }

    fn path(&self, inode: u64) -> crate::Result<LogicalPath> {
        self.inode_to_path
            .read()
            .get(&inode)
            .cloned()
            .ok_or_else(|| CensorFsError::new(ErrorCode::NotFound, "unknown inode"))
    }

    fn child(&self, parent: u64, name: &OsStr) -> crate::Result<LogicalPath> {
        use std::os::unix::ffi::OsStrExt;
        let parent = self.path(parent)?;
        let bytes = if parent.is_root() {
            name.as_bytes().to_vec()
        } else {
            let mut bytes = parent.as_bytes().to_vec();
            bytes.push(b'/');
            bytes.extend_from_slice(name.as_bytes());
            bytes
        };
        LogicalPath::new(bytes)
    }

    fn attr(&self, entry: &ManifestEntry) -> FileAttr {
        let kind = match entry.kind {
            EntryKind::File => FileType::RegularFile,
            EntryKind::Directory => FileType::Directory,
        };
        let atime = UNIX_EPOCH + Duration::from_nanos(entry.atime_ns);
        let mtime = UNIX_EPOCH + Duration::from_nanos(entry.mtime_ns);
        FileAttr {
            ino: self.inode(&entry.path),
            size: entry.size,
            blocks: entry.size.div_ceil(512),
            atime,
            mtime,
            ctime: mtime,
            crtime: UNIX_EPOCH,
            kind,
            perm: (entry.mode & 0o777) as u16,
            nlink: if entry.kind == EntryKind::Directory {
                2
            } else {
                1
            },
            uid: self
                .engine
                .view(self.view_id)
                .map(|view| view.owner_uid)
                .unwrap_or(0),
            gid: self
                .engine
                .view(self.view_id)
                .map(|view| view.owner_gid)
                .unwrap_or(0),
            rdev: 0,
            blksize: 4096,
            flags: 0,
        }
    }

    fn directory_snapshot(&self, inode: u64) -> crate::Result<Vec<(u64, FileType, String)>> {
        let path = self.path(inode)?;
        let entries = self.engine.readdir(self.view_id, &path)?;
        let mut rows = Vec::with_capacity(entries.len() + 2);
        rows.push((inode, FileType::Directory, ".".to_owned()));
        let parent = path
            .parent()
            .map(|value| self.inode(&value))
            .unwrap_or(inode);
        rows.push((parent, FileType::Directory, "..".to_owned()));
        rows.extend(entries.into_iter().map(|entry| {
            let kind = if entry.kind == EntryKind::Directory {
                FileType::Directory
            } else {
                FileType::RegularFile
            };
            (
                self.inode(&entry.path),
                kind,
                String::from_utf8_lossy(entry.path.file_name()).into_owned(),
            )
        }));
        Ok(rows)
    }
}

pub fn spawn_session(
    engine: Arc<ViewEngine>,
    view_id: ViewId,
    descriptor: std::os::fd::OwnedFd,
) -> crate::Result<()> {
    let adapter = FuseViewAdapter::new(engine, view_id)?;
    fuser::Session::from_fd(adapter, descriptor, fuser::SessionACL::All)
        .spawn()
        .map_err(Into::into)
        .map(|_background| ())
}

fn errno(error: &CensorFsError) -> i32 {
    match error.code {
        ErrorCode::BadRequest => libc::EINVAL,
        ErrorCode::NotFound => libc::ENOENT,
        ErrorCode::AccessDenied => libc::EACCES,
        ErrorCode::ReadOnly => libc::EROFS,
        ErrorCode::AlreadyExistsDifferent => libc::EEXIST,
        ErrorCode::BusyOpenWriters => libc::EBUSY,
        ErrorCode::StateChanged | ErrorCode::HeadChanged => libc::EBUSY,
        ErrorCode::DirectoryNotEmpty => libc::ENOTEMPTY,
        ErrorCode::Unsupported => libc::EOPNOTSUPP,
        ErrorCode::NoSpace => libc::ENOSPC,
        _ => libc::EIO,
    }
}

impl Filesystem for FuseViewAdapter {
    fn init(
        &mut self,
        _request: &Request<'_>,
        _config: &mut KernelConfig,
    ) -> std::result::Result<(), libc::c_int> {
        Ok(())
    }

    fn lookup(&mut self, _request: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let result = self
            .child(parent, name)
            .and_then(|path| self.engine.lookup(self.view_id, &path));
        match result {
            Ok(entry) => reply.entry(&self.ttl(), &self.attr(&entry), 0),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn getattr(&mut self, _request: &Request<'_>, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        let result = self
            .path(ino)
            .and_then(|path| self.engine.lookup(self.view_id, &path));
        match result {
            Ok(entry) => reply.attr(&self.ttl(), &self.attr(&entry)),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn open(&mut self, _request: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        let wants_write = flags & (libc::O_WRONLY | libc::O_RDWR) != 0;
        if wants_write && !self.writable {
            return reply.error(libc::EROFS);
        }
        let path = match self.path(ino) {
            Ok(value) => value,
            Err(error) => return reply.error(errno(&error)),
        };
        let handle = match self.engine.open_file(self.view_id, &path, wants_write) {
            Ok(value) => value,
            Err(error) => return reply.error(errno(&error)),
        };
        let open_flags = if self.writable {
            fuser::consts::FOPEN_DIRECT_IO
        } else {
            fuser::consts::FOPEN_KEEP_CACHE
        };
        reply.opened(handle, open_flags);
    }

    fn read(
        &mut self,
        _request: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        let result = self.path(ino).and_then(|path| {
            if offset < 0 {
                return Err(CensorFsError::bad_request("negative read offset"));
            }
            self.engine
                .read_handle(self.view_id, _fh, &path, offset as u64, size)
        });
        match result {
            Ok(data) => reply.data(&data),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn write(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyWrite,
    ) {
        if offset < 0 {
            return reply.error(libc::EINVAL);
        }
        match self
            .engine
            .write_handle(self.view_id, fh, offset as u64, data)
        {
            Ok(size) => reply.written(size),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn release(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        _flags: i32,
        _lock_owner: Option<u64>,
        _flush: bool,
        reply: ReplyEmpty,
    ) {
        match self.engine.release_file(self.view_id, fh) {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn readdir(
        &mut self,
        _request: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let result = if _fh == 0 {
            self.directory_snapshot(ino)
        } else {
            self.directories
                .lock()
                .get(&_fh)
                .cloned()
                .ok_or_else(|| CensorFsError::not_found("directory handle", _fh))
        };
        match result {
            Ok(rows) => {
                for (index, (inode, kind, name)) in
                    rows.into_iter().enumerate().skip(offset.max(0) as usize)
                {
                    if reply.add(inode, (index + 1) as i64, kind, name) {
                        break;
                    }
                }
                reply.ok();
            }
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn opendir(&mut self, _request: &Request<'_>, ino: u64, _flags: i32, reply: ReplyOpen) {
        match self.directory_snapshot(ino) {
            Ok(snapshot) => {
                let handle = self.next_directory.fetch_add(1, Ordering::SeqCst);
                self.directories.lock().insert(handle, snapshot);
                reply.opened(handle, 0);
            }
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn releasedir(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        fh: u64,
        _flags: i32,
        reply: ReplyEmpty,
    ) {
        if self.directories.lock().remove(&fh).is_some() {
            reply.ok();
        } else {
            reply.error(libc::EBADF);
        }
    }

    fn mkdir(
        &mut self,
        _request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        let result = self.child(parent, name).and_then(|path| {
            self.engine.mkdir(self.view_id, path.clone(), mode)?;
            self.engine.lookup(self.view_id, &path)
        });
        match result {
            Ok(entry) => reply.entry(&ZERO_TTL, &self.attr(&entry), 0),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn create(
        &mut self,
        _request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        mode: u32,
        _umask: u32,
        flags: i32,
        reply: ReplyCreate,
    ) {
        let result = self.child(parent, name).and_then(|path| {
            self.engine
                .create_file(self.view_id, path.clone(), &[], mode, true)?;
            let handle = self.engine.open_file(
                self.view_id,
                &path,
                flags & (libc::O_WRONLY | libc::O_RDWR) != 0,
            )?;
            let entry = self.engine.lookup(self.view_id, &path)?;
            Ok((entry, handle))
        });
        match result {
            Ok((entry, handle)) => reply.created(
                &ZERO_TTL,
                &self.attr(&entry),
                0,
                handle,
                fuser::consts::FOPEN_DIRECT_IO,
            ),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn unlink(&mut self, _request: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let result = self
            .child(parent, name)
            .and_then(|path| self.engine.unlink(self.view_id, path));
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn rmdir(&mut self, _request: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        let result = self
            .child(parent, name)
            .and_then(|path| self.engine.rmdir(self.view_id, path));
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn rename(
        &mut self,
        _request: &Request<'_>,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        flags: u32,
        reply: ReplyEmpty,
    ) {
        if flags != 0 {
            return reply.error(libc::EOPNOTSUPP);
        }
        let result = self.child(parent, name).and_then(|old| {
            self.child(newparent, newname)
                .and_then(|new| self.engine.rename(self.view_id, old, new))
        });
        match result {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn link(
        &mut self,
        _request: &Request<'_>,
        ino: u64,
        newparent: u64,
        newname: &OsStr,
        reply: ReplyEntry,
    ) {
        // Hard link: copy-up source content into a fresh object at the target
        // (CensorFS has no cross-path inode sharing), then return the target's attr.
        let result = self.path(ino).and_then(|source| {
            self.child(newparent, newname).and_then(|target| {
                self.engine.link(self.view_id, source, target.clone())?;
                self.engine.lookup(self.view_id, &target)
            })
        });
        match result {
            Ok(entry) => reply.entry(&self.ttl(), &self.attr(&entry), 0),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn fsync(
        &mut self,
        _request: &Request<'_>,
        _ino: u64,
        _fh: u64,
        _datasync: bool,
        reply: ReplyEmpty,
    ) {
        match self.engine.fsync(self.view_id) {
            Ok(()) => reply.ok(),
            Err(error) => reply.error(errno(&error)),
        }
    }

    fn setattr(
        &mut self,
        _request: &Request<'_>,
        ino: u64,
        mode: Option<u32>,
        uid: Option<u32>,
        gid: Option<u32>,
        size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        let result = self.path(ino).and_then(|path| {
            if uid.is_some() || gid.is_some() {
                return Err(CensorFsError::new(
                    ErrorCode::Unsupported,
                    "chown is not supported",
                ));
            }
            if let Some(size) = size {
                if let Some(handle) = fh.filter(|handle| *handle != 0) {
                    self.engine.truncate_handle(self.view_id, handle, size)?;
                } else {
                    self.engine.truncate(self.view_id, path.clone(), size)?;
                }
            }
            if let Some(mode) = mode {
                self.engine.chmod(self.view_id, path.clone(), mode)?;
            }
            if atime.is_some() || mtime.is_some() {
                let current = self.engine.lookup(self.view_id, &path)?;
                let to_ns = |value: TimeOrNow| {
                    let time = match value {
                        TimeOrNow::SpecificTime(value) => value,
                        TimeOrNow::Now => SystemTime::now(),
                    };
                    time.duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos()
                        .min(u64::MAX as u128) as u64
                };
                self.engine.utimens(
                    self.view_id,
                    path.clone(),
                    atime.map(to_ns).unwrap_or(current.atime_ns),
                    mtime.map(to_ns).unwrap_or(current.mtime_ns),
                )?;
            }
            self.engine.lookup(self.view_id, &path)
        });
        match result {
            Ok(entry) => reply.attr(&self.ttl(), &self.attr(&entry)),
            Err(error) => reply.error(errno(&error)),
        }
    }
}
