//! A local administrator supplies immutable program definitions. Only vault
//! references cross the agent boundary; program input/output is local.
use crate::{
    error::{Error, Result},
    references::Reference,
    vault::VaultEntry,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use zeroize::Zeroizing;

const MAX_REFS: usize = 256;
pub(crate) const MAX_IO: usize = 256 * 1024;
const MAX_VALUE: usize = 16 * 1024;
pub const COMPUTE: &str = "private_compute";

#[derive(Default)]
pub(crate) struct Registry {
    programs: BTreeMap<String, Arc<Program>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    tools: Vec<Program>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Program {
    name: String,
    description: String,
    command: Vec<String>,
    output: OutputKind,
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum OutputKind {
    Order,
    Values,
}

fn default_timeout() -> u64 {
    3000
}

fn valid_name(name: &str) -> bool {
    let Some(suffix) = name.strip_prefix("private_") else {
        return false;
    };
    name != COMPUTE
        && name.len() <= 64
        && suffix
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase())
        && suffix
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

impl Registry {
    pub(crate) fn from_json(raw: &str) -> Result<Self> {
        let manifest: Manifest = crate::json::from_slice(raw.as_bytes())
            .and_then(|value| serde_json::from_value(value).map_err(|_| Error::PolicyInvalid))?;
        if manifest.tools.len() > 32 {
            return Err(Error::PolicyInvalid);
        }
        let mut programs = BTreeMap::new();
        for program in manifest.tools {
            if !valid_name(&program.name)
                || !(1..=512).contains(&program.description.trim().len())
                || program.description.chars().any(char::is_control)
                || !(1..=16).contains(&program.command.len())
                || !Path::new(&program.command[0]).is_absolute()
                || !Path::new(&program.command[0]).is_file()
                || program
                    .command
                    .iter()
                    .any(|arg| arg.len() > 4096 || arg.contains('\0'))
                || !(100..=30_000).contains(&program.timeout_ms)
                || programs
                    .insert(program.name.clone(), Arc::new(program))
                    .is_some()
            {
                return Err(Error::PolicyInvalid);
            }
        }
        Ok(Self { programs })
    }

    pub(crate) fn program(&self, name: &str) -> Option<Arc<Program>> {
        self.programs.get(name).cloned()
    }

    pub(crate) fn catalog(&self) -> Value {
        let mut tools = vec![json!({"name": COMPUTE, "kind": "builtin"})];
        tools.extend(self.programs.values().map(|program| {
            json!({"name": program.name, "kind": "program", "description": program.description, "output": program.output})
        }));
        json!({"tools": tools})
    }
}

pub(crate) fn references(payload: &Value) -> Result<Vec<Reference>> {
    let obj = payload.as_object().ok_or(Error::ComputeInvalid)?;
    let refs = obj
        .get("refs")
        .and_then(Value::as_array)
        .filter(|refs| obj.len() == 1 && (1..=MAX_REFS).contains(&refs.len()))
        .ok_or(Error::ComputeInvalid)?;
    refs.iter()
        .map(|value| Reference::parse(value.as_str().ok_or(Error::InvalidReference)?))
        .collect()
}

pub(crate) struct Invocation {
    pub(crate) program: Arc<Program>,
    pub(crate) epoch: u64,
    pub(crate) refs: Vec<Reference>,
    pub(crate) entries: Vec<VaultEntry>,
}

pub(crate) enum Output {
    Order(Vec<usize>),
    Values(Vec<Zeroizing<String>>),
}

impl Invocation {
    pub(crate) fn execute(&self, helper: PathBuf) -> Result<Output> {
        #[derive(Serialize)]
        struct Operand<'a> {
            value: &'a str,
            entity_type: &'a str,
            unit: Option<&'a str>,
        }
        #[derive(Serialize)]
        struct Input<'a> {
            inputs: Vec<Operand<'a>>,
        }
        let capacity: usize = self.entries.iter().map(|entry| entry.value.len()).sum();
        if capacity > MAX_IO {
            return Err(Error::ResourceLimit);
        }
        let input = Zeroizing::new(
            serde_json::to_vec(&Input {
                inputs: self
                    .entries
                    .iter()
                    .map(|entry| Operand {
                        value: entry.value.as_str(),
                        entity_type: &entry.entity_type,
                        unit: entry.metadata.unit.as_deref(),
                    })
                    .collect(),
            })
            .map_err(|_| Error::PrivateToolFailed)?,
        );
        if input.len() > MAX_IO {
            return Err(Error::ResourceLimit);
        }
        let output = run(helper, &self.program, &input)?;
        self.parse_output(&output)
    }

    fn parse_output(&self, bytes: &[u8]) -> Result<Output> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Order {
            order: Vec<usize>,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Values {
            values: Vec<String>,
        }
        match self.program.output {
            OutputKind::Order => {
                let Order { order } =
                    serde_json::from_slice(bytes).map_err(|_| Error::PrivateToolFailed)?;
                if order.len() != self.refs.len() {
                    return Err(Error::PrivateToolFailed);
                }
                let mut seen = vec![false; order.len()];
                for &index in &order {
                    let used = seen.get_mut(index).ok_or(Error::PrivateToolFailed)?;
                    if std::mem::replace(used, true) {
                        return Err(Error::PrivateToolFailed);
                    }
                }
                Ok(Output::Order(order))
            }
            OutputKind::Values => {
                let Values { values } =
                    serde_json::from_slice(bytes).map_err(|_| Error::PrivateToolFailed)?;
                let values: Vec<_> = values.into_iter().map(Zeroizing::new).collect();
                if !(1..=MAX_REFS).contains(&values.len())
                    || values.iter().any(|value| {
                        !(1..=MAX_VALUE).contains(&value.len()) || value.contains("<CP_")
                    })
                {
                    return Err(Error::PrivateToolFailed);
                }
                Ok(Output::Values(values))
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn run(helper: PathBuf, program: &Program, input: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    use censorprivacy_runtime_sys as sys;
    use std::{
        io::{ErrorKind, Read, Write},
        process::{Child, Command, Stdio},
        time::{Duration, Instant},
    };
    struct Session(Child, bool);
    impl Drop for Session {
        fn drop(&mut self) {
            if !self.1 {
                let _ = sys::kill_session(&self.0);
                let _ = self.0.wait();
            }
        }
    }

    let end = Instant::now() + Duration::from_millis(program.timeout_ms);
    let mut command = Command::new(helper);
    command
        .arg("private-program-worker")
        .arg("--")
        .args(&program.command)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LC_ALL", "C.UTF-8")
        .current_dir("/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = Session(
        sys::spawn_private_program(&mut command).map_err(|_| Error::PrivateToolFailed)?,
        false,
    );
    let pidfd = sys::pidfd_open(child.0.id()).map_err(|_| Error::PrivateToolFailed)?;
    let mut stdin = child.0.stdin.take();
    let mut stdout = child.0.stdout.take().ok_or(Error::PrivateToolFailed)?;
    sys::set_nonblocking(stdin.as_ref().ok_or(Error::PrivateToolFailed)?)
        .and_then(|()| sys::set_nonblocking(&stdout))
        .map_err(|_| Error::PrivateToolFailed)?;
    let mut sent = 0;
    let mut eof = false;
    let mut success = false;
    let mut output = Zeroizing::new(Vec::new());
    let mut buffer = Zeroizing::new([0u8; 8192]);
    loop {
        if Instant::now() >= end {
            return Err(Error::RuntimeTimeout);
        }
        if let Some(pipe) = &mut stdin {
            match pipe.write(&input[sent..]) {
                Ok(count) => sent += count,
                Err(err)
                    if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) => {}
                Err(_) => return Err(Error::PrivateToolFailed),
            }
            if sent == input.len() {
                stdin = None; // Signal end of JSON input.
            }
        }
        while !eof {
            match stdout.read(&mut *buffer) {
                Ok(0) => eof = true,
                Ok(count) if output.len() + count <= MAX_IO => {
                    output.extend_from_slice(&buffer[..count])
                }
                Ok(_) => return Err(Error::ResourceLimit),
                Err(err)
                    if matches!(err.kind(), ErrorKind::WouldBlock | ErrorKind::Interrupted) =>
                {
                    break
                }
                Err(_) => return Err(Error::PrivateToolFailed),
            }
        }
        if !child.1 && sys::exited_unreaped(&pidfd).map_err(|_| Error::PrivateToolFailed)? {
            // Clean up pipes held by background descendants before reaping the
            // leader, which pins the process group ID until cleanup is sent.
            sys::kill_session(&child.0).map_err(|_| Error::RuntimeCleanupFailed)?;
            success = child
                .0
                .wait()
                .map_err(|_| Error::PrivateToolFailed)?
                .success();
            child.1 = true;
            if !success || sent != input.len() {
                return Err(Error::PrivateToolFailed);
            }
        }
        if child.1 && eof && success {
            return Ok(output);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[cfg(not(target_os = "linux"))]
fn run(_: PathBuf, _: &Program, _: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    Err(Error::PrivateToolFailed)
}
