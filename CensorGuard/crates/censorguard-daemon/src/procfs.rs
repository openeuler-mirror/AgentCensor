use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::io;

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
    let path = format!("/proc/{pid}/task/{pid}/children");
    let data = match fs::read_to_string(path) {
        Ok(data) => data,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    data.split_whitespace()
        .map(|value| {
            value
                .parse()
                .map_err(|_| invalid_data(format!("invalid child pid {value:?}")))
        })
        .collect()
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_has_a_start_time() -> Result<(), io::Error> {
        assert!(start_time(std::process::id())? > 0);
        let _uid = owner_uid(std::process::id())?;
        Ok(())
    }
}
