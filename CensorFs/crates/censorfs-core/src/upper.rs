use crate::error::{ErrorCode, Result, CensorFsError};
use std::fs::File;
#[cfg(not(target_os = "linux"))]
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct UpperDirectory {
    path: PathBuf,
    #[cfg(target_os = "linux")]
    directory: File,
}

impl UpperDirectory {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        std::fs::create_dir_all(&path)?;
        #[cfg(target_os = "linux")]
        let directory = File::open(&path)?;
        Ok(Self {
            path,
            #[cfg(target_os = "linux")]
            directory,
        })
    }

    pub fn write_new(&self, data: &[u8]) -> Result<(String, u64)> {
        for _ in 0..8 {
            let name = uuid::Uuid::new_v4().simple().to_string();
            match self.open_child(&name, true) {
                Ok(mut file) => {
                    file.write_all(data)?;
                    file.sync_all()?;
                    return Ok((name, data.len() as u64));
                }
                Err(error) if error.code == ErrorCode::AlreadyExistsDifferent => continue,
                Err(error) => return Err(error),
            }
        }
        Err(CensorFsError::new(
            ErrorCode::AlreadyExistsDifferent,
            "could not allocate a unique upper object name",
        ))
    }

    pub fn read(&self, name: &str) -> Result<Vec<u8>> {
        let mut file = self.open_child(name, false)?;
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;
        Ok(data)
    }

    pub fn sync_names<'a>(&self, names: impl IntoIterator<Item = &'a str>) -> Result<()> {
        for name in names {
            #[cfg(target_os = "linux")]
            self.open_child(name, false)?.sync_all()?;
            #[cfg(not(target_os = "linux"))]
            {
                Self::validate_name(name)?;
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(self.path.join(name))?
                    .sync_all()?;
            }
        }
        #[cfg(target_os = "linux")]
        self.directory.sync_all()?;
        Ok(())
    }

    fn validate_name(name: &str) -> Result<()> {
        if name.len() != 32 || !name.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(CensorFsError::corrupt(
                "upper object has an invalid storage name",
            ));
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn open_child(&self, name: &str, create: bool) -> Result<File> {
        use std::ffi::CString;
        use std::os::fd::{AsRawFd, FromRawFd};

        Self::validate_name(name)?;
        let name = CString::new(name).unwrap();
        #[repr(C)]
        struct OpenHow {
            flags: u64,
            mode: u64,
            resolve: u64,
        }
        const RESOLVE_NO_XDEV: u64 = 0x01;
        const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
        const RESOLVE_NO_SYMLINKS: u64 = 0x04;
        const RESOLVE_BENEATH: u64 = 0x08;
        let flags = if create {
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC
        } else {
            libc::O_RDONLY | libc::O_CLOEXEC
        };
        let how = OpenHow {
            flags: flags as u64,
            mode: if create { 0o600 } else { 0 },
            resolve: RESOLVE_BENEATH
                | RESOLVE_NO_MAGICLINKS
                | RESOLVE_NO_XDEV
                | RESOLVE_NO_SYMLINKS,
        };
        let descriptor = unsafe {
            libc::syscall(
                libc::SYS_openat2,
                self.directory.as_raw_fd(),
                name.as_ptr(),
                &how,
                std::mem::size_of::<OpenHow>(),
            ) as libc::c_int
        };
        if descriptor < 0 {
            let error = std::io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::EEXIST) => Err(CensorFsError::new(
                    ErrorCode::AlreadyExistsDifferent,
                    error.to_string(),
                )),
                Some(libc::ENOSYS) => Err(CensorFsError::new(
                    ErrorCode::Unsupported,
                    "openat2 is required",
                )),
                _ => Err(error.into()),
            };
        }
        Ok(unsafe { File::from_raw_fd(descriptor) })
    }

    #[cfg(not(target_os = "linux"))]
    fn open_child(&self, name: &str, create: bool) -> Result<File> {
        Self::validate_name(name)?;
        let mut options = OpenOptions::new();
        options.read(!create).write(create);
        if create {
            options.create_new(true);
        }
        options.open(self.path.join(name)).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                CensorFsError::new(ErrorCode::AlreadyExistsDifferent, error.to_string())
            } else {
                error.into()
            }
        })
    }

    pub fn path_for_diagnostics(&self) -> &Path {
        &self.path
    }
}
