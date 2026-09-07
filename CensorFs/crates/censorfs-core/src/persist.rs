use crate::codec::{self, RecordKind};
use crate::error::{ErrorCode, Result, CensorFsError};
use crate::fault::FaultInjector;
use crate::ids::*;
use crate::model::{JournalRecord, PersistedResponse};
use fs2::FileExt;
use serde::{de::DeserializeOwned, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub const SUBDIRS: &[&str] = &[
    "superblock",
    "branches",
    "generations",
    "manifests",
    "objects",
    "txs",
    "tickets",
    "candidates",
    "publish",
    "requests",
    "journal",
    "tmp",
    "trash",
];

#[derive(Debug)]
pub struct InstanceLock {
    file: File,
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

#[derive(Debug, Clone)]
pub struct Persistence {
    root: Arc<PathBuf>,
    fault: Arc<FaultInjector>,
    journal_seq: Arc<AtomicU64>,
}

impl Persistence {
    pub fn new(root: impl Into<PathBuf>, fault: Arc<FaultInjector>) -> Self {
        Self {
            root: Arc::new(root.into()),
            fault,
            journal_seq: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn fault(&self) -> &Arc<FaultInjector> {
        &self.fault
    }

    pub fn initialize_layout(&self) -> Result<()> {
        fs::create_dir_all(self.root())?;
        self.restrict_directory(self.root())?;
        for dir in SUBDIRS {
            let path = self.root().join(dir);
            fs::create_dir_all(&path)?;
            self.restrict_directory(&path)?;
        }
        self.sync_dir(self.root())?;
        Ok(())
    }

    fn restrict_directory(&self, path: &Path) -> Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
        #[cfg(not(unix))]
        let _ = path;
        Ok(())
    }

    pub fn acquire_lock(&self) -> Result<InstanceLock> {
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(self.root().join("lock"))?;
        file.try_lock_exclusive().map_err(|e| {
            CensorFsError::new(
                ErrorCode::StateChanged,
                format!("instance already locked: {e}"),
            )
        })?;
        Ok(InstanceLock { file })
    }

    pub fn acquire_existing_lock(&self) -> Result<InstanceLock> {
        let path = self.root().join("lock");
        let file = OpenOptions::new().read(true).write(true).open(&path)?;
        file.try_lock_exclusive().map_err(|error| {
            CensorFsError::new(
                ErrorCode::StateChanged,
                format!("instance already locked: {error}"),
            )
        })?;
        Ok(InstanceLock { file })
    }

    pub fn validate_backing_filesystem(&self) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            if std::env::var_os("CENSORFS_ALLOW_UNSUPPORTED_BACKING").is_some() {
                return Ok(());
            }
            use std::os::unix::ffi::OsStrExt;
            let c_path = std::ffi::CString::new(self.root().as_os_str().as_bytes())
                .map_err(|_| CensorFsError::bad_request("storage root contains NUL"))?;
            let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
            if unsafe { libc::statfs(c_path.as_ptr(), &mut stat) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            const EXT4_SUPER_MAGIC: libc::c_long = 0xEF53;
            const XFS_SUPER_MAGIC: libc::c_long = 0x58465342;
            if stat.f_type != EXT4_SUPER_MAGIC && stat.f_type != XFS_SUPER_MAGIC {
                return Err(CensorFsError::new(
                    ErrorCode::Unsupported,
                    format!(
                        "backing filesystem magic 0x{:x} is not XFS/ext4",
                        stat.f_type
                    ),
                ));
            }
        }
        Ok(())
    }

    pub fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.root().join(relative)
    }
    pub fn branch_dir(&self, id: &BranchId) -> PathBuf {
        self.path("branches").join(id.storage_name())
    }
    pub fn generation_path(&self, id: GenerationId) -> PathBuf {
        self.path("generations").join(format!("{id}.meta"))
    }
    pub fn manifest_path(&self, id: ManifestId) -> PathBuf {
        self.path("manifests").join(format!("{id}.manifest"))
    }
    pub fn object_path(&self, id: ObjectId) -> PathBuf {
        self.path("objects").join(format!("{id}.data"))
    }
    pub fn tx_path(&self, id: TxId) -> PathBuf {
        self.path("txs").join(format!("{id}.meta"))
    }
    pub fn ticket_dir(&self, id: TicketId) -> PathBuf {
        self.path("tickets").join(id.to_string())
    }
    pub fn ticket_meta_path(&self, id: TicketId) -> PathBuf {
        self.ticket_dir(id).join("ticket.meta")
    }
    pub fn candidate_path(&self, id: CandidateId) -> PathBuf {
        self.path("candidates").join(format!("{id}.meta"))
    }
    pub fn merge_intent_path(&self, id: CandidateId) -> PathBuf {
        self.path("candidates").join(format!("{id}.merge"))
    }
    pub fn request_path(&self, id: RequestId) -> PathBuf {
        self.path("requests").join(format!("{id}.result"))
    }
    pub fn receipt_path(&self, branch: &BranchId, seq: u64) -> PathBuf {
        self.path("publish")
            .join(branch.storage_name())
            .join(format!("{seq}.receipt"))
    }

    pub fn write_record<T: Serialize>(
        &self,
        path: &Path,
        kind: RecordKind,
        value: &T,
        create_only: bool,
    ) -> Result<()> {
        let bytes = codec::encode_record(kind, value)?;
        self.write_atomic(path, &bytes, create_only)
    }

    pub fn read_record<T: DeserializeOwned>(&self, path: &Path, kind: RecordKind) -> Result<T> {
        let bytes = fs::read(path)?;
        codec::decode_record(kind, &bytes)
    }

    pub fn write_atomic(&self, path: &Path, bytes: &[u8], create_only: bool) -> Result<()> {
        if create_only && path.exists() {
            return Err(CensorFsError::new(
                ErrorCode::AlreadyExistsDifferent,
                format!("{} already exists", path.display()),
            ));
        }
        let parent = path
            .parent()
            .ok_or_else(|| CensorFsError::bad_request("record has no parent directory"))?;
        fs::create_dir_all(parent)?;
        let tmp = self.path("tmp").join(format!(
            "{}.{}.tmp",
            path.file_name().unwrap_or_default().to_string_lossy(),
            uuid::Uuid::new_v4()
        ));
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        let mut file = options.open(&tmp)?;
        use std::io::Write;
        file.write_all(bytes)?;
        self.fault.checkpoint("atomic.after_write")?;
        file.sync_all()?;
        self.fault.checkpoint("atomic.after_file_fsync")?;
        drop(file);
        #[cfg(target_os = "linux")]
        if create_only {
            use std::os::unix::ffi::OsStrExt;
            let source = std::ffi::CString::new(tmp.as_os_str().as_bytes())
                .map_err(|_| CensorFsError::bad_request("temporary path contains NUL"))?;
            let target = std::ffi::CString::new(path.as_os_str().as_bytes())
                .map_err(|_| CensorFsError::bad_request("record path contains NUL"))?;
            let result = unsafe {
                libc::syscall(
                    libc::SYS_renameat2,
                    libc::AT_FDCWD,
                    source.as_ptr(),
                    libc::AT_FDCWD,
                    target.as_ptr(),
                    libc::RENAME_NOREPLACE,
                )
            };
            if result != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::EEXIST) {
                    let _ = fs::remove_file(&tmp);
                    return Err(CensorFsError::new(
                        ErrorCode::AlreadyExistsDifferent,
                        format!("{} already exists", path.display()),
                    ));
                }
                return Err(error.into());
            }
        } else {
            fs::rename(&tmp, path)?;
        }
        #[cfg(not(target_os = "linux"))]
        {
            #[cfg(windows)]
            if !create_only && path.exists() {
                fs::remove_file(path)?;
            }
            fs::rename(&tmp, path)?;
        }
        self.fault.checkpoint("atomic.after_rename")?;
        self.sync_dir(parent)?;
        self.fault.checkpoint("atomic.after_dir_fsync")?;
        Ok(())
    }

    pub fn sync_dir(&self, path: &Path) -> Result<()> {
        #[cfg(unix)]
        {
            File::open(path)?.sync_all()?;
        }
        #[cfg(not(unix))]
        {
            let _ = path;
        }
        Ok(())
    }

    pub fn append_journal(&self, mut record: JournalRecord) -> Result<JournalRecord> {
        let path = self.path("journal/current.log");
        record.seq = self.journal_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(path)?;
        codec::write_journal_frame(&mut file, &record)?;
        self.fault.checkpoint("journal.after_append")?;
        file.sync_data()?;
        self.fault.checkpoint("journal.after_fsync")?;
        Ok(record)
    }

    pub fn recover_journal(&self) -> Result<Vec<JournalRecord>> {
        let path = self.path("journal/current.log");
        if !path.exists() {
            return Ok(Vec::new());
        }
        let mut file = OpenOptions::new().read(true).write(true).open(&path)?;
        let (records, valid_end) = codec::read_journal_frames::<JournalRecord>(&mut file)?;
        let actual = file.seek(SeekFrom::End(0))?;
        if valid_end < actual {
            file.set_len(valid_end)?;
            file.sync_all()?;
        }
        self.journal_seq
            .store(records.last().map(|r| r.seq).unwrap_or(0), Ordering::SeqCst);
        Ok(records)
    }

    pub fn inspect_journal(&self) -> Result<(Vec<JournalRecord>, u64, u64)> {
        let path = self.path("journal/current.log");
        if !path.exists() {
            return Ok((Vec::new(), 0, 0));
        }
        let mut file = OpenOptions::new().read(true).open(path)?;
        let actual = file.metadata()?.len();
        let (records, valid_end) = codec::read_journal_frames::<JournalRecord>(&mut file)?;
        Ok((records, valid_end, actual))
    }

    pub fn save_response<T: Serialize>(
        &self,
        id: RequestId,
        operation: &str,
        value: &T,
    ) -> Result<()> {
        let response = PersistedResponse {
            operation: operation.to_owned(),
            payload: codec::serialize(value)?,
        };
        self.write_record(
            &self.request_path(id),
            RecordKind::RequestResult,
            &response,
            true,
        )
    }

    pub fn load_response<T: DeserializeOwned>(
        &self,
        id: RequestId,
        operation: &str,
    ) -> Result<Option<T>> {
        let path = self.request_path(id);
        if !path.exists() {
            return Ok(None);
        }
        let response: PersistedResponse = self.read_record(&path, RecordKind::RequestResult)?;
        if response.operation != operation {
            return Err(CensorFsError::new(
                ErrorCode::AlreadyExistsDifferent,
                "request_id was already used for another operation",
            ));
        }
        Ok(Some(codec::deserialize(&response.payload)?))
    }
}
