use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt};

const MAX_DESCENDANTS: usize = 4095;

pub struct ProcessInfo {
    pub ppid: u32,
    pub comm: String,
}

pub fn start_time(pid: u32) -> Result<u64, io::Error> {
    let path = format!("/proc/{pid}/stat");
    let stat = fs::read_to_string(&path)?;
    let close = stat
        .rfind(')')
        .ok_or_else(|| invalid_data(format!("malformed {path}: missing comm terminator")))?;
    let fields: Vec<_> = stat[close + 1..].split_whitespace().collect();
    fields
        .get(19)
        .ok_or_else(|| invalid_data(format!("malformed {path}: missing starttime")))?
        .parse()
        .map_err(|_| invalid_data(format!("malformed {path}: invalid starttime")))
}

pub fn descendants(root: u32) -> Result<Vec<u32>, io::Error> {
    let mut found = BTreeSet::new();
    let mut queue = VecDeque::from([root]);
    while let Some(pid) = queue.pop_front() {
        for child in direct_children(pid)? {
            if found.insert(child) {
                if found.len() > MAX_DESCENDANTS {
                    return Err(invalid_data(format!(
                        "process tree rooted at {root} exceeds {MAX_DESCENDANTS} descendants"
                    )));
                }
                queue.push_back(child);
            }
        }
    }
    Ok(found.into_iter().collect())
}

pub fn owner_uid(pid: u32) -> Result<u32, io::Error> {
    let path = format!("/proc/{pid}/status");
    let status = fs::read_to_string(&path)?;
    let uid_line = status
        .lines()
        .find(|line| line.starts_with("Uid:"))
        .ok_or_else(|| invalid_data(format!("malformed {path}: missing Uid")))?;
    uid_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| invalid_data(format!("malformed {path}: missing real uid")))?
        .parse()
        .map_err(|_| invalid_data(format!("malformed {path}: invalid real uid")))
}

/// Include pre-existing and inherited sockets when a process is attached.
pub fn socket_inodes(pid: u32) -> Result<Vec<u64>, io::Error> {
    let entries = match fs::read_dir(format!("/proc/{pid}/fd")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            return Err(socket_access_error(pid, error));
        }
        Err(error) => return Err(error),
    };
    let mut inodes = BTreeSet::new();
    for entry in entries {
        match fs::metadata(entry?.path()) {
            Ok(metadata) if metadata.file_type().is_socket() => {
                inodes.insert(metadata.ino());
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                return Err(socket_access_error(pid, error));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(inodes.into_iter().collect())
}

fn socket_access_error(pid: u32, error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!(
            "cannot inspect /proc/{pid}/fd; cross-UID socket registration requires CAP_SYS_PTRACE"
        ),
    )
}

pub fn process_info(pid: u32) -> Result<ProcessInfo, io::Error> {
    let path = format!("/proc/{pid}/stat");
    let stat = fs::read_to_string(&path)?;
    let open = stat
        .find('(')
        .ok_or_else(|| invalid_data(format!("malformed {path}: missing comm start")))?;
    let close = stat
        .rfind(')')
        .ok_or_else(|| invalid_data(format!("malformed {path}: missing comm end")))?;
    if close <= open {
        return Err(invalid_data(format!("malformed {path}: invalid comm")));
    }
    let fields: Vec<_> = stat[close + 1..].split_whitespace().collect();
    let ppid = fields
        .get(1)
        .ok_or_else(|| invalid_data(format!("malformed {path}: missing ppid")))?
        .parse()
        .map_err(|_| invalid_data(format!("malformed {path}: invalid ppid")))?;
    Ok(ProcessInfo {
        ppid,
        comm: stat[open + 1..close].to_owned(),
    })
}

fn direct_children(pid: u32) -> Result<Vec<u32>, io::Error> {
    let tasks = match fs::read_dir(format!("/proc/{pid}/task")) {
        Ok(tasks) => tasks,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut children = BTreeSet::new();
    // Linux records children on their creating thread, including non-leader threads.
    for task in tasks {
        let task = match task {
            Ok(task) => task,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let data = match fs::read_to_string(task.path().join("children")) {
            Ok(data) => data,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for value in data.split_whitespace() {
            children.insert(
                value
                    .parse()
                    .map_err(|_| invalid_data(format!("invalid child pid {value:?}")))?,
            );
        }
    }
    Ok(children.into_iter().collect())
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command};
    use std::sync::mpsc;
    use std::thread;

    struct ChildGuard(Child);

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn descendants_include_children_created_by_non_leader_threads() -> Result<(), io::Error> {
        let (child_sender, child_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        thread::scope(|scope| -> Result<(), io::Error> {
            let worker = scope.spawn(move || -> Result<(), io::Error> {
                let child = ChildGuard(Command::new("sleep").arg("30").spawn()?);
                child_sender.send(child.0.id()).map_err(io::Error::other)?;
                let _ = release_receiver.recv();
                Ok(())
            });
            let child_pid = child_receiver.recv().map_err(io::Error::other)?;
            let children = direct_children(std::process::id());
            let descendants = descendants(std::process::id());
            let _ = release_sender.send(());
            worker
                .join()
                .map_err(|_| io::Error::other("child creator thread panicked"))??;
            assert!(children?.contains(&child_pid));
            assert!(descendants?.contains(&child_pid));
            Ok(())
        })
    }

    #[test]
    fn current_process_has_a_start_time() -> Result<(), io::Error> {
        assert!(start_time(std::process::id())? > 0);
        let _uid = owner_uid(std::process::id())?;
        Ok(())
    }
}
