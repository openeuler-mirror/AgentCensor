//! Local, opt-in Agent conversation history. Never sent to the provider.
use crate::{
    detection::Span,
    error::{Error, Result},
    references::random_hex,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

const MAX_RECORD: u64 = 32 * 1024 * 1024;

pub fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

/// Model-hidden client metadata has its own namespace, independent of legacy
/// xiaoO IDs. Never use an external session identifier as a filesystem path.
pub fn agent_session_id(agent: &str, session: &str) -> Result<String> {
    let valid = |value: &str, limit| {
        !value.is_empty()
            && value.len() <= limit
            && value
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.:".contains(&c))
    };
    if !valid(agent, 64) || !valid(session, 128) {
        return Err(Error::ProtocolBlocked);
    }
    let mut hash = Sha256::new();
    hash.update(b"censorprivacy-conversation-v1\0");
    hash.update(agent.as_bytes());
    hash.update(b"\0");
    hash.update(session.as_bytes());
    use std::fmt::Write;
    let mut id = String::with_capacity(32);
    for byte in &hash.finalize()[..16] {
        write!(&mut id, "{byte:02x}").expect("writing to a String");
    }
    Ok(id)
}

fn legacy_agent() -> String {
    "xiaoo".into()
}

#[derive(Clone, Serialize, Deserialize)]
pub struct TextSnapshot {
    pub path: String,
    pub text: String,
    pub analysis: Value,
}
impl TextSnapshot {
    pub(crate) fn new(path: &str, text: &str, masked: &str, spans: &[Span]) -> Self {
        let categories: BTreeSet<_> = spans.iter().map(|s| &s.entity_type).collect();
        let results: Vec<_> = spans.iter().map(|s| json!({
            "entity_type":s.entity_type,
            "start":text[..s.start].chars().count(),
            "end":text[..s.end].chars().count(),
            "score":s.metadata.get("score").and_then(|s| s.parse::<f64>().ok()),
            "sources":s.metadata.get("source").map(|s| s.split(',').collect::<Vec<_>>()).unwrap_or(vec!["policy"]),
            "validation":s.metadata.get("validation").map(String::as_str).unwrap_or("not_checked"),
            "mask_eligible":true,
        })).collect();
        Self {
            path: path.into(),
            text: text.into(),
            analysis: json!({"recorded":true,"results":results,"count":spans.len(),
                "category_count":categories.len(),"masked_text":masked,"masked_count":spans.len(),
                "elapsed_ms":0,"mask_threshold":null,"mask_style":"gateway"}),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Exchange {
    pub id: String,
    pub session_id: String,
    // Old records predate this field and were exclusively written by xiaoO.
    #[serde(default = "legacy_agent")]
    pub agent: String,
    pub created_at: u64,
    pub updated_at: u64,
    pub model: String,
    pub status: String,
    pub error: Option<String>,
    pub messages: Vec<Value>,
    pub snapshots: Vec<TextSnapshot>,
    pub response: Value,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub struct ConversationStore {
    directory: PathBuf,
    writes: Mutex<()>,
}
impl ConversationStore {
    pub fn open(directory: PathBuf) -> Result<Self> {
        if !directory.is_absolute() {
            return Err(Error::ServiceUnavailable);
        }
        if !directory.exists() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&directory)
                .map_err(|_| Error::ServiceUnavailable)?;
        }
        Self::private_directory(&directory)?;
        Ok(Self {
            directory,
            writes: Mutex::new(()),
        })
    }
    fn private_directory(path: &Path) -> Result<()> {
        let metadata = fs::symlink_metadata(path).map_err(|_| Error::ServiceUnavailable)?;
        if !metadata.is_dir() || metadata.mode() & 0o077 != 0 {
            return Err(Error::ServiceUnavailable);
        }
        Ok(())
    }
    fn session_path(&self, session: &str) -> Result<PathBuf> {
        if !valid_id(session) {
            return Err(Error::ProtocolBlocked);
        }
        Self::private_directory(&self.directory)?;
        Ok(self.directory.join(session))
    }
    // Serialize readers, reset, and writers, including a standalone Web process
    // opening the same records directory. Never remove or replace this lock file.
    fn locked<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        let _guard = self.writes.lock().map_err(|_| Error::ServiceUnavailable)?;
        Self::private_directory(&self.directory)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(self.directory.join(".records.lock"))
            .map_err(|_| Error::ServiceUnavailable)?;
        let metadata = lock.metadata().map_err(|_| Error::ServiceUnavailable)?;
        if !metadata.is_file() || metadata.mode() & 0o077 != 0 {
            return Err(Error::ServiceUnavailable);
        }
        censorprivacy_runtime_sys::lock_file_exclusive(&lock)
            .map_err(|_| Error::ServiceUnavailable)?;
        // Closing the file releases flock on success, error, or unwinding.
        operation()
    }
    fn read(&self, path: &Path) -> Result<Exchange> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .map_err(|_| Error::ServiceUnavailable)?;
        let metadata = file.metadata().map_err(|_| Error::ServiceUnavailable)?;
        if !metadata.is_file() || metadata.mode() & 0o077 != 0 || metadata.len() > MAX_RECORD {
            return Err(Error::ResourceLimit);
        }
        let mut bytes = Vec::new();
        file.take(MAX_RECORD + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| Error::ServiceUnavailable)?;
        if bytes.len() as u64 > MAX_RECORD {
            return Err(Error::ResourceLimit);
        }
        serde_json::from_slice(&bytes).map_err(|_| Error::InvalidJson)
    }
    fn write(&self, record: &Exchange) -> Result<()> {
        self.locked(|| self.write_locked(record, false))
    }
    fn write_locked(&self, record: &Exchange, create: bool) -> Result<()> {
        let directory = self.session_path(&record.session_id)?;
        let destination = directory.join(format!("{}.json", record.id));
        if !create {
            match fs::symlink_metadata(&destination) {
                // A reset revoked this recorder. Continue serving the Agent,
                // but never recreate its deleted exchange, even if a new
                // request has since reused the same session directory.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Ok(metadata) if metadata.is_file() => (),
                _ => return Err(Error::ServiceUnavailable),
            }
        }
        Self::private_directory(&directory)?;
        let bytes = serde_json::to_vec(record).map_err(|_| Error::InvalidJson)?;
        if bytes.len() as u64 > MAX_RECORD {
            return Err(Error::ResourceLimit);
        }
        let temporary = directory.join(format!(".{}.tmp", random_hex::<16>()?));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(&temporary)
                .map_err(|_| Error::ServiceUnavailable)?;
            file.write_all(&bytes)
                .map_err(|_| Error::ServiceUnavailable)?;
            fs::rename(&temporary, &destination).map_err(|_| Error::ServiceUnavailable)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }
    pub fn exchanges(&self, session: &str) -> Result<Vec<Exchange>> {
        self.locked(|| self.exchanges_locked(session))
    }
    fn exchanges_locked(&self, session: &str) -> Result<Vec<Exchange>> {
        let directory = self.session_path(session)?;
        Self::private_directory(&directory)?;
        let mut records = Vec::new();
        for entry in fs::read_dir(directory).map_err(|_| Error::ServiceUnavailable)? {
            let entry = entry.map_err(|_| Error::ServiceUnavailable)?;
            let name = entry.file_name();
            let name = name.to_str().unwrap_or("");
            if name.strip_suffix(".json").is_some_and(valid_id) {
                records.push(self.read(&entry.path())?);
            }
        }
        records.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        Ok(records)
    }
    pub fn sessions(&self) -> Result<Vec<Value>> {
        self.locked(|| self.sessions_locked())
    }
    fn sessions_locked(&self) -> Result<Vec<Value>> {
        Self::private_directory(&self.directory)?;
        let mut sessions = Vec::new();
        for entry in fs::read_dir(&self.directory).map_err(|_| Error::ServiceUnavailable)? {
            let entry = entry.map_err(|_| Error::ServiceUnavailable)?;
            let id = entry.file_name().to_string_lossy().into_owned();
            if !valid_id(&id) {
                continue;
            }
            let records = self.exchanges_locked(&id)?;
            if let (Some(first), Some(last)) = (records.first(), records.last()) {
                let title = first
                    .messages
                    .iter()
                    .find(|m| m["role"] == "user")
                    .map(|m| match &m["content"] {
                        Value::String(text) => text.clone(),
                        Value::Array(parts) => parts
                            .iter()
                            .filter_map(|p| p["text"].as_str())
                            .collect::<Vec<_>>()
                            .join(" "),
                        _ => String::new(),
                    })
                    .filter(|text| !text.trim().is_empty())
                    .unwrap_or_else(|| "Agent 会话".into());
                sessions.push(
                    json!({"id":id,"title":title.chars().take(80).collect::<String>(),
                    "agent":first.agent,
                    "created_at":first.created_at,"updated_at":last.updated_at,
                    "status":last.status,"exchange_count":records.len()}),
                );
            }
        }
        sessions.sort_by_key(|s| std::cmp::Reverse(s["updated_at"].as_u64().unwrap_or(0)));
        Ok(sessions)
    }
    /// Delete only this store's session directories. Completed and in-flight
    /// exchanges disappear together; unrelated files and gateway state survive.
    pub fn clear(&self) -> Result<usize> {
        self.locked(|| {
            let mut cleared = 0;
            for entry in fs::read_dir(&self.directory).map_err(|_| Error::ServiceUnavailable)? {
                let entry = entry.map_err(|_| Error::ServiceUnavailable)?;
                if !valid_id(&entry.file_name().to_string_lossy()) {
                    continue;
                }
                Self::private_directory(&entry.path())?;
                fs::remove_dir_all(entry.path()).map_err(|_| Error::ServiceUnavailable)?;
                cleared += 1;
            }
            Ok(cleared)
        })
    }
    /// Only the gateway writer calls this on startup; standalone readers must
    /// not change the status of an active gateway's exchanges.
    pub fn recover_interrupted(&self) -> Result<()> {
        self.locked(|| {
            for session in self.sessions_locked()? {
                for mut record in
                    self.exchanges_locked(session["id"].as_str().ok_or(Error::InvalidJson)?)?
                {
                    if matches!(record.status.as_str(), "pending" | "streaming") {
                        record.status = "error".into();
                        record.error = Some("gateway_restarted".into());
                        self.write_locked(&record, false)?;
                    }
                }
            }
            Ok(())
        })
    }
    pub fn begin(self: &Arc<Self>, session: &str, payload: &Value) -> Result<ExchangeRecorder> {
        self.begin_for_agent(session, "xiaoo", payload)
    }
    pub fn begin_for_agent(
        self: &Arc<Self>,
        session: &str,
        agent: &str,
        payload: &Value,
    ) -> Result<ExchangeRecorder> {
        self.locked(|| self.begin_locked(session, agent, payload))
    }
    fn begin_locked(
        self: &Arc<Self>,
        session: &str,
        agent: &str,
        payload: &Value,
    ) -> Result<ExchangeRecorder> {
        let directory = self.session_path(session)?;
        if !directory.exists() {
            match fs::DirBuilder::new().mode(0o700).create(&directory) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(_) => return Err(Error::ServiceUnavailable),
            }
        }
        Self::private_directory(&directory)?;
        let record = Exchange {
            id: random_hex::<16>()?,
            session_id: session.into(),
            agent: agent.into(),
            created_at: now(),
            updated_at: now(),
            model: payload["model"].as_str().unwrap_or("").into(),
            status: "pending".into(),
            error: None,
            messages: payload["messages"].as_array().cloned().unwrap_or_default(),
            snapshots: vec![],
            response: json!({"role":"assistant","content":"","tool_calls":[]}),
        };
        self.write_locked(&record, true)?;
        Ok(ExchangeRecorder {
            store: self.clone(),
            record,
            pending: Vec::new(),
            last_write: Instant::now(),
            terminal: false,
        })
    }
}

pub struct ExchangeRecorder {
    store: Arc<ConversationStore>,
    record: Exchange,
    pending: Vec<u8>,
    last_write: Instant,
    terminal: bool,
}
impl ExchangeRecorder {
    fn save(&mut self) -> Result<()> {
        self.record.updated_at = now();
        self.store.write(&self.record)?;
        self.last_write = Instant::now();
        Ok(())
    }
    pub fn protected(&mut self, snapshots: Vec<TextSnapshot>) -> Result<()> {
        self.record.snapshots = snapshots;
        self.record.status = "streaming".into();
        self.save()
    }
    /// Consume already-restored SSE bytes; split UTF-8, references and tools have
    /// already passed the gateway's transformer before reaching this observer.
    pub fn feed(&mut self, bytes: &[u8]) -> Result<()> {
        self.pending.extend_from_slice(bytes);
        while let Some(end) = self.pending.iter().position(|c| *c == b'\n') {
            let line: Vec<_> = self.pending.drain(..=end).collect();
            let line = std::str::from_utf8(&line)
                .map_err(|_| Error::InvalidJson)?
                .trim_end();
            if let Some(data) = line.strip_prefix("data:") {
                let data = data.trim();
                if data == "[DONE]" || data.is_empty() {
                    continue;
                }
                let value: Value = serde_json::from_str(data).map_err(|_| Error::InvalidJson)?;
                if let Some(choices) = value["choices"].as_array() {
                    for choice in choices
                        .iter()
                        .filter(|c| c["index"].as_u64().unwrap_or(0) == 0)
                    {
                        self.delta(&choice["delta"])?;
                    }
                }
            }
        }
        if self.pending.len() as u64 > MAX_RECORD {
            return Err(Error::ResourceLimit);
        }
        if self.last_write.elapsed().as_millis() >= 150 {
            self.save()?;
        }
        Ok(())
    }
    fn delta(&mut self, delta: &Value) -> Result<()> {
        if let Some(content) = delta["content"].as_str() {
            let previous = self.record.response["content"].as_str().unwrap_or("");
            if previous.len() + content.len() > MAX_RECORD as usize {
                return Err(Error::ResourceLimit);
            }
            self.record.response["content"] = json!(format!("{previous}{content}"));
        }
        if let Some(calls) = delta["tool_calls"].as_array() {
            for call in calls {
                let index = call["index"].as_u64().unwrap_or(0) as usize;
                if index >= 128 {
                    return Err(Error::ResourceLimit);
                }
                let tools = self.record.response["tool_calls"]
                    .as_array_mut()
                    .ok_or(Error::InvalidJson)?;
                while tools.len() <= index {
                    tools.push(
                        json!({"id":"","type":"function","function":{"name":"","arguments":""}}),
                    );
                }
                for (source, target) in [
                    ("/id", "/id"),
                    ("/function/name", "/function/name"),
                    ("/function/arguments", "/function/arguments"),
                ] {
                    if let Some(part) = call.pointer(source).and_then(Value::as_str) {
                        let field = tools[index].pointer_mut(target).ok_or(Error::InvalidJson)?;
                        *field = json!(format!("{}{part}", field.as_str().unwrap_or("")));
                    }
                }
            }
        }
        Ok(())
    }
    pub fn complete(&mut self, response: Option<&Value>) -> Result<()> {
        if let Some(message) = response.and_then(|r| r.pointer("/choices/0/message")) {
            self.record.response = message.clone();
        }
        self.record.status = "completed".into();
        self.save()?;
        self.terminal = true;
        Ok(())
    }
    pub fn fail(&mut self, code: &str) {
        self.record.status = "error".into();
        self.record.error = Some(code.into());
        let _ = self.save();
        self.terminal = true;
    }
}
impl Drop for ExchangeRecorder {
    fn drop(&mut self) {
        if !self.terminal {
            self.fail("interrupted");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_xiaoo_records_and_multipart_agent_titles_remain_readable() {
        let directory =
            std::env::temp_dir().join(format!("cp-legacy-{}", random_hex::<16>().unwrap()));
        let store = Arc::new(ConversationStore::open(directory.clone()).unwrap());
        let session = "0123456789abcdef0123456789abcdef";
        store
            .begin(
                session,
                &json!({"messages":[{"role":"user","content":"old xiaoO history"}]}),
            )
            .unwrap()
            .complete(None)
            .unwrap();
        let record = &store.exchanges(session).unwrap()[0];
        let path = directory.join(session).join(format!("{}.json", record.id));
        let mut legacy = serde_json::to_value(record).unwrap();
        legacy.as_object_mut().unwrap().remove("agent");
        fs::write(path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert_eq!(store.exchanges(session).unwrap()[0].agent, "xiaoo");
        let id = agent_session_id("other-agent", "external-session").unwrap();
        store
            .begin_for_agent(
                &id,
                "other-agent",
                &json!({"messages":[{"role":"user","content":[
            {"type":"text","text":"multipart"},{"type":"text","text":"title"}]}]}),
            )
            .unwrap()
            .complete(None)
            .unwrap();
        let sessions = store.sessions().unwrap();
        assert_eq!(
            sessions.iter().find(|s| s["id"] == id).unwrap()["title"],
            "multipart title"
        );
        assert_eq!(
            sessions.iter().find(|s| s["id"] == session).unwrap()["agent"],
            "xiaoo"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn clear_persists_and_revokes_inflight_writers_even_when_a_session_is_reused() {
        let directory =
            std::env::temp_dir().join(format!("cp-clear-{}", random_hex::<16>().unwrap()));
        let store = Arc::new(ConversationStore::open(directory.clone()).unwrap());
        let session = "0123456789abcdef0123456789abcdef";
        let second = "fedcba9876543210fedcba9876543210";
        let payload = json!({"messages":[{"role":"user","content":"old recording"}]});
        store
            .begin(session, &payload)
            .unwrap()
            .complete(None)
            .unwrap();
        let mut inflight = store.begin(session, &payload).unwrap();
        let interrupted = store.begin(second, &payload).unwrap();
        fs::write(directory.join("unrelated.txt"), "keep").unwrap();

        // Standalone Web readers have another store handle, just as another
        // process does; clearing must also revoke the gateway's active writer.
        let reader = ConversationStore::open(directory.clone()).unwrap();
        assert_eq!(reader.clear().unwrap(), 2);
        assert!(store.sessions().unwrap().is_empty());
        let mut fresh = store
            .begin(
                session,
                &json!({"messages":[{"role":"user","content":"new test"}]}),
            )
            .unwrap();
        inflight.protected(vec![]).unwrap();
        inflight.complete(None).unwrap();
        drop(interrupted);
        fresh.complete(None).unwrap();
        let records = reader.exchanges(session).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].messages[0]["content"], "new test");
        assert_eq!(records[0].status, "completed");
        assert!(!directory.join(second).exists());
        assert_eq!(reader.clear().unwrap(), 1);
        assert_eq!(reader.clear().unwrap(), 0);
        let reopened = ConversationStore::open(directory.clone()).unwrap();
        reopened.recover_interrupted().unwrap();
        assert!(reopened.sessions().unwrap().is_empty());
        assert_eq!(
            fs::read_to_string(directory.join("unrelated.txt")).unwrap(),
            "keep"
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn clear_does_not_follow_links_outside_the_recording_directory() {
        let directory =
            std::env::temp_dir().join(format!("cp-clear-{}", random_hex::<16>().unwrap()));
        let outside = directory.with_extension("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("keep.txt"), "private unrelated data").unwrap();
        let store = Arc::new(ConversationStore::open(directory.clone()).unwrap());
        let session = "0123456789abcdef0123456789abcdef";
        store
            .begin(session, &json!({}))
            .unwrap()
            .complete(None)
            .unwrap();
        std::os::unix::fs::symlink(&outside, directory.join(session).join("external")).unwrap();
        assert_eq!(store.clear().unwrap(), 1);
        assert_eq!(
            fs::read_to_string(outside.join("keep.txt")).unwrap(),
            "private unrelated data"
        );
        std::os::unix::fs::symlink(&outside, directory.join(session)).unwrap();
        assert!(store.clear().is_err());
        assert!(outside.join("keep.txt").exists());
        fs::remove_dir_all(directory).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }
}
