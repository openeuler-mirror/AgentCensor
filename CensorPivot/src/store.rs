use crate::error::{PivotError, Result};
use crate::model::{SCHEMA_VERSION, TransactionRecord};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct TransactionStore {
    root: PathBuf,
}

impl TransactionStore {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn load(&self, transaction_id: Uuid) -> Result<TransactionRecord> {
        let bytes = fs::read(self.path(transaction_id))?;
        let record: TransactionRecord = serde_json::from_slice(&bytes)?;
        validate_record(&record)?;
        Ok(record)
    }

    pub fn find_request(&self, request_id: &str) -> Result<Option<TransactionRecord>> {
        for record in self.list()? {
            if record.request_id == request_id {
                return Ok(Some(record));
            }
        }
        Ok(None)
    }

    pub fn list(&self) -> Result<Vec<TransactionRecord>> {
        let mut records = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let bytes = fs::read(&path)?;
            let record: TransactionRecord = serde_json::from_slice(&bytes).map_err(|error| {
                PivotError::Protocol(format!(
                    "invalid transaction file {}: {error}",
                    path.display()
                ))
            })?;
            validate_record(&record)?;
            records.push(record);
        }
        records.sort_by_key(|record| record.created_at_ms);
        Ok(records)
    }

    pub fn save(&self, record: &TransactionRecord) -> Result<()> {
        validate_record(record)?;
        let target = self.path(record.transaction_id);
        let temp = self
            .root
            .join(format!(".{}.{}.tmp", record.transaction_id, Uuid::new_v4()));
        let bytes = serde_json::to_vec_pretty(record)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        let write_result = (|| -> std::io::Result<()> {
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()
        })();
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temp);
            return Err(error.into());
        }
        fs::rename(&temp, &target)?;
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }

    fn path(&self, transaction_id: Uuid) -> PathBuf {
        self.root.join(format!("{transaction_id}.json"))
    }
}

fn validate_record(record: &TransactionRecord) -> Result<()> {
    if record.schema_version != SCHEMA_VERSION {
        return Err(PivotError::Protocol(format!(
            "unsupported transaction schema {}",
            record.schema_version
        )));
    }
    match (record.decision, record.state) {
        (None, crate::model::TransactionState::CommitDecided)
        | (None, crate::model::TransactionState::AbortDecided)
        | (None, crate::model::TransactionState::Committed)
        | (None, crate::model::TransactionState::Aborted) => Err(PivotError::Protocol(
            "decided transaction is missing its decision".into(),
        )),
        (Some(_), crate::model::TransactionState::Received)
        | (Some(_), crate::model::TransactionState::Preparing)
        | (Some(_), crate::model::TransactionState::Prepared) => Err(PivotError::Protocol(
            "pre-decision state unexpectedly contains a decision".into(),
        )),
        (Some(crate::model::Decision::Commit), crate::model::TransactionState::AbortDecided)
        | (Some(crate::model::Decision::Commit), crate::model::TransactionState::Aborted)
        | (Some(crate::model::Decision::Abort), crate::model::TransactionState::CommitDecided)
        | (Some(crate::model::Decision::Abort), crate::model::TransactionState::Committed) => Err(
            PivotError::Protocol("transaction state contradicts durable decision".into()),
        ),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    fn record() -> TransactionRecord {
        TransactionRecord {
            schema_version: SCHEMA_VERSION,
            transaction_id: Uuid::new_v4(),
            request_id: "request-1".into(),
            request_fingerprint: "hash".into(),
            actor_uid: 1000,
            state: TransactionState::Received,
            decision: None,
            summary: RequestSummary {
                session_id: "session".into(),
                trace_id: None,
                branch: "main".into(),
                expected_generation: "gen".into(),
                expected_head_seq: 1,
                guard_scope: "scope".into(),
                guard_group: String::new(),
                call_ids: vec!["call".into()],
            },
            fs: FsResources::default(),
            open_request_id: Uuid::new_v4(),
            prepare_request_id: Uuid::new_v4(),
            finish_request_id: Uuid::new_v4(),
            results: Vec::new(),
            warnings: Vec::new(),
            last_error: None,
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    #[test]
    fn atomically_round_trips_record() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let store = TransactionStore::open(temp.path())?;
        let record = record();
        store.save(&record)?;
        assert_eq!(store.load(record.transaction_id)?.request_id, "request-1");
        Ok(())
    }

    #[test]
    fn rejects_state_that_contradicts_decision() {
        let mut value = record();
        value.decision = Some(Decision::Commit);
        value.state = TransactionState::Aborted;
        assert!(validate_record(&value).is_err());
    }
}
