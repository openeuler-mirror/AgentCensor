use crate::codec::RecordKind;
use crate::error::{ErrorCode, Result, CensorFsError};
use crate::ids::*;
use crate::model::*;
use crate::persist::Persistence;
use crate::upper::UpperDirectory;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};

#[derive(Debug, Clone)]
pub struct ManifestMergePlan {
    pub merge_base: GenerationId,
    pub entries: BTreeMap<LogicalPath, ManifestEntry>,
    pub conflicts: Vec<MergeConflict>,
    pub already_up_to_date: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ObjectRecord {
    object_id: ObjectId,
    content_digest: Digest,
    data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct VersionStore {
    persist: Persistence,
}

impl VersionStore {
    pub fn new(persist: Persistence) -> Self {
        Self { persist }
    }

    pub fn write_object(&self, data: &[u8]) -> Result<(ObjectId, Digest)> {
        let object_id = ObjectId::new();
        let digest = *blake3::hash(data).as_bytes();
        let record = ObjectRecord {
            object_id,
            content_digest: digest,
            data: data.to_vec(),
        };
        self.persist.write_record(
            &self.persist.object_path(object_id),
            RecordKind::Object,
            &record,
            true,
        )?;
        self.persist.fault().checkpoint("object.durable")?;
        let verified = self.read_object(object_id, digest)?;
        if verified.len() != data.len() {
            return Err(CensorFsError::corrupt("new object length changed"));
        }
        Ok((object_id, digest))
    }

    pub fn read_object(&self, object_id: ObjectId, expected: Digest) -> Result<Vec<u8>> {
        let record: ObjectRecord = self
            .persist
            .read_record(&self.persist.object_path(object_id), RecordKind::Object)?;
        if record.object_id != object_id
            || record.content_digest != expected
            || blake3::hash(&record.data).as_bytes() != &expected
        {
            return Err(CensorFsError::corrupt(format!(
                "object {object_id} failed validation"
            )));
        }
        Ok(record.data)
    }

    pub fn validate_object(&self, object_id: ObjectId) -> Result<()> {
        let record: ObjectRecord = self
            .persist
            .read_record(&self.persist.object_path(object_id), RecordKind::Object)?;
        if record.object_id != object_id
            || blake3::hash(&record.data).as_bytes() != &record.content_digest
        {
            return Err(CensorFsError::corrupt(format!(
                "object {object_id} failed validation"
            )));
        }
        Ok(())
    }

    pub fn write_manifest(&self, manifest: &Manifest) -> Result<(ManifestId, Digest)> {
        Self::validate_manifest(manifest)?;
        let manifest_id = ManifestId::new();
        let bytes = crate::codec::serialize(manifest)?;
        let digest = *blake3::hash(&bytes).as_bytes();
        self.persist.write_record(
            &self.persist.manifest_path(manifest_id),
            RecordKind::Manifest,
            manifest,
            true,
        )?;
        self.persist.fault().checkpoint("manifest.durable")?;
        Ok((manifest_id, digest))
    }

    pub fn load_manifest(&self, meta: &GenerationMeta) -> Result<Manifest> {
        let manifest: Manifest = self.persist.read_record(
            &self.persist.manifest_path(meta.manifest_id),
            RecordKind::Manifest,
        )?;
        let digest = *blake3::hash(&crate::codec::serialize(&manifest)?).as_bytes();
        if digest != meta.manifest_digest
            || manifest.generation_id != meta.generation_id
            || manifest.entries.len() as u64 != meta.entry_count
        {
            return Err(CensorFsError::corrupt(format!(
                "generation {} manifest metadata mismatch",
                meta.generation_id
            )));
        }
        Self::validate_manifest(&manifest)?;
        Ok(manifest)
    }

    pub fn write_generation(&self, meta: &GenerationMeta) -> Result<()> {
        self.persist.write_record(
            &self.persist.generation_path(meta.generation_id),
            RecordKind::Generation,
            meta,
            true,
        )?;
        self.persist.fault().checkpoint("generation.durable")
    }

    pub fn load_generation(&self, id: GenerationId) -> Result<(GenerationMeta, Manifest)> {
        let path = self.persist.generation_path(id);
        if !path.exists() {
            return Err(CensorFsError::not_found("generation", id));
        }
        let meta: GenerationMeta = self.persist.read_record(&path, RecordKind::Generation)?;
        if meta.generation_id != id {
            return Err(CensorFsError::corrupt("generation id mismatch"));
        }
        let manifest = self.load_manifest(&meta)?;
        for entry in manifest
            .entries
            .values()
            .filter(|e| e.kind == EntryKind::File)
        {
            let object_id = entry
                .object_id
                .ok_or_else(|| CensorFsError::corrupt("file has no object"))?;
            let digest = entry
                .content_digest
                .ok_or_else(|| CensorFsError::corrupt("file has no digest"))?;
            let data = self.read_object(object_id, digest)?;
            if data.len() as u64 != entry.size {
                return Err(CensorFsError::corrupt("object size mismatch"));
            }
        }
        Ok((meta, manifest))
    }

    pub fn load_generation_meta(&self, id: GenerationId) -> Result<GenerationMeta> {
        self.load_generation(id).map(|value| value.0)
    }

    pub fn build_generation(
        &self,
        base: GenerationId,
        overlay: &BTreeMap<LogicalPath, OverlayEntry>,
        ticket_upper: &UpperDirectory,
        source_ticket: Option<TicketId>,
        candidate_id: CandidateId,
        kind: GenerationKind,
        rollback_target: Option<GenerationId>,
    ) -> Result<GenerationMeta> {
        let (_, base_manifest) = self.load_generation(base)?;
        let generation_id = GenerationId::new();
        let mut entries = base_manifest.entries;

        for (path, change) in overlay {
            match change {
                OverlayEntry::Deleted => {
                    let victims: Vec<_> = entries
                        .keys()
                        .filter(|p| *p == path || p.starts_with_dir(path))
                        .cloned()
                        .collect();
                    for victim in victims {
                        entries.remove(&victim);
                    }
                }
                OverlayEntry::Directory {
                    mode,
                    atime_ns,
                    mtime_ns,
                } => {
                    entries.insert(
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
                    storage_name,
                    mode,
                    atime_ns,
                    mtime_ns,
                    ..
                } => {
                    let data = ticket_upper.read(storage_name)?;
                    let (object_id, digest) = self.write_object(&data)?;
                    entries.insert(
                        path.clone(),
                        ManifestEntry {
                            path: path.clone(),
                            kind: EntryKind::File,
                            mode: *mode,
                            size: data.len() as u64,
                            atime_ns: *atime_ns,
                            mtime_ns: *mtime_ns,
                            object_id: Some(object_id),
                            content_digest: Some(digest),
                        },
                    );
                }
            }
        }

        let manifest = Manifest {
            generation_id,
            entries,
        };
        let (manifest_id, manifest_digest) = self.write_manifest(&manifest)?;
        let meta = GenerationMeta {
            generation_id,
            manifest_id,
            parents: vec![base],
            source_ticket,
            source_candidate: Some(candidate_id),
            kind,
            rollback_target,
            manifest_digest,
            entry_count: manifest.entries.len() as u64,
        };
        self.write_generation(&meta)?;
        self.load_generation(generation_id)?;
        Ok(meta)
    }

    pub fn clone_generation_contents(
        &self,
        current: GenerationId,
        target: GenerationId,
        candidate_id: CandidateId,
    ) -> Result<GenerationMeta> {
        let (_, target_manifest) = self.load_generation(target)?;
        self.load_generation(current)?;
        let generation_id = GenerationId::new();
        let manifest = Manifest {
            generation_id,
            entries: target_manifest.entries,
        };
        let (manifest_id, manifest_digest) = self.write_manifest(&manifest)?;
        let meta = GenerationMeta {
            generation_id,
            manifest_id,
            parents: vec![current],
            source_ticket: None,
            source_candidate: Some(candidate_id),
            kind: GenerationKind::Rollback,
            rollback_target: Some(target),
            manifest_digest,
            entry_count: manifest.entries.len() as u64,
        };
        self.write_generation(&meta)?;
        self.load_generation(generation_id)?;
        Ok(meta)
    }

    pub fn plan_merge(
        &self,
        source: GenerationId,
        target: GenerationId,
    ) -> Result<ManifestMergePlan> {
        if source == target {
            return Ok(ManifestMergePlan {
                merge_base: source,
                entries: self.load_generation(source)?.1.entries,
                conflicts: Vec::new(),
                already_up_to_date: true,
            });
        }
        let merge_base = self.unique_merge_base(source, target)?;
        let base = self.load_generation(merge_base)?.1;
        let source_manifest = self.load_generation(source)?.1;
        let target_manifest = self.load_generation(target)?.1;
        let paths: BTreeSet<_> = base
            .entries
            .keys()
            .chain(source_manifest.entries.keys())
            .chain(target_manifest.entries.keys())
            .cloned()
            .collect();
        let mut entries = BTreeMap::new();
        let mut conflicts = BTreeMap::new();
        for path in paths {
            let base_entry = base.entries.get(&path);
            let source_entry = source_manifest.entries.get(&path);
            let target_entry = target_manifest.entries.get(&path);
            let selected = if source_entry == target_entry {
                source_entry.cloned()
            } else if source_entry == base_entry {
                target_entry.cloned()
            } else if target_entry == base_entry {
                source_entry.cloned()
            } else {
                let reason = match (source_entry, target_entry) {
                    (None, Some(_)) | (Some(_), None) => MergeConflictReason::DeleteModify,
                    (Some(source), Some(target)) if source.kind != target.kind => {
                        MergeConflictReason::TypeChanged
                    }
                    _ => MergeConflictReason::BothModified,
                };
                conflicts.insert(path.clone(), reason);
                target_entry.cloned()
            };
            if let Some(entry) = selected {
                entries.insert(path, entry);
            }
        }

        let structural_paths: Vec<_> = entries.keys().cloned().collect();
        for path in structural_paths {
            if path.is_root() {
                if entries
                    .get(&path)
                    .is_some_and(|entry| entry.kind != EntryKind::Directory)
                {
                    conflicts.insert(path, MergeConflictReason::TypeChanged);
                }
                continue;
            }
            let parent = path.parent().unwrap();
            if !entries
                .get(&parent)
                .is_some_and(|entry| entry.kind == EntryKind::Directory)
            {
                conflicts.insert(path, MergeConflictReason::ParentMissingOrNotDirectory);
            }
        }
        if !entries.contains_key(&LogicalPath::root()) {
            conflicts.insert(
                LogicalPath::root(),
                MergeConflictReason::ParentMissingOrNotDirectory,
            );
        }
        let conflicts = conflicts
            .into_iter()
            .map(|(path, reason)| MergeConflict { path, reason })
            .collect();
        Ok(ManifestMergePlan {
            merge_base,
            entries,
            conflicts,
            already_up_to_date: false,
        })
    }

    pub fn write_merge_generation(
        &self,
        target: GenerationId,
        source: GenerationId,
        entries: BTreeMap<LogicalPath, ManifestEntry>,
        candidate_id: CandidateId,
    ) -> Result<GenerationMeta> {
        self.load_generation(target)?;
        self.load_generation(source)?;
        let generation_id = GenerationId::new();
        let manifest = Manifest {
            generation_id,
            entries,
        };
        let (manifest_id, manifest_digest) = self.write_manifest(&manifest)?;
        let meta = GenerationMeta {
            generation_id,
            manifest_id,
            parents: vec![target, source],
            source_ticket: None,
            source_candidate: Some(candidate_id),
            kind: GenerationKind::Merge,
            rollback_target: None,
            manifest_digest,
            entry_count: manifest.entries.len() as u64,
        };
        self.write_generation(&meta)?;
        self.load_generation(generation_id)?;
        Ok(meta)
    }

    fn unique_merge_base(
        &self,
        source: GenerationId,
        target: GenerationId,
    ) -> Result<GenerationId> {
        let source_ancestors = self.ancestor_distances(source)?;
        let target_ancestors = self.ancestor_distances(target)?;
        let common: Vec<_> = source_ancestors
            .keys()
            .filter(|id| target_ancestors.contains_key(id))
            .copied()
            .collect();
        if common.is_empty() {
            return Err(CensorFsError::new(
                ErrorCode::Conflict,
                "branches have no common ancestor",
            ));
        }
        let mut nearest = Vec::new();
        'candidate: for candidate in &common {
            for other in &common {
                if candidate != other && self.is_ancestor(*candidate, *other)? {
                    continue 'candidate;
                }
            }
            nearest.push(*candidate);
        }
        if nearest.len() != 1 {
            return Err(CensorFsError::new(
                ErrorCode::Conflict,
                "merge base is ambiguous",
            ));
        }
        Ok(nearest[0])
    }

    fn ancestor_distances(&self, start: GenerationId) -> Result<HashMap<GenerationId, u64>> {
        let mut distances = HashMap::from([(start, 0_u64)]);
        let mut queue = VecDeque::from([(start, 0_u64)]);
        while let Some((generation, distance)) = queue.pop_front() {
            for parent in self.load_generation_meta(generation)?.parents {
                let next = distance
                    .checked_add(1)
                    .ok_or_else(|| CensorFsError::corrupt("generation depth overflow"))?;
                if distances
                    .get(&parent)
                    .is_none_or(|existing| next < *existing)
                {
                    distances.insert(parent, next);
                    queue.push_back((parent, next));
                }
            }
        }
        Ok(distances)
    }

    fn is_ancestor(&self, ancestor: GenerationId, node: GenerationId) -> Result<bool> {
        let mut pending = vec![node];
        let mut visited = HashSet::new();
        while let Some(current) = pending.pop() {
            if current == ancestor {
                return Ok(true);
            }
            if visited.insert(current) {
                pending.extend(self.load_generation_meta(current)?.parents);
            }
        }
        Ok(false)
    }

    pub fn diff(&self, left: GenerationId, right: GenerationId) -> Result<Vec<PathDiff>> {
        let (_, left) = self.load_generation(left)?;
        let (_, right) = self.load_generation(right)?;
        let paths: BTreeSet<_> = left
            .entries
            .keys()
            .chain(right.entries.keys())
            .cloned()
            .collect();
        let mut result = Vec::new();
        for path in paths {
            let kind = match (left.entries.get(&path), right.entries.get(&path)) {
                (None, Some(_)) => Some(DiffKind::Added),
                (Some(_), None) => Some(DiffKind::Deleted),
                (Some(a), Some(b)) if a.kind != b.kind => Some(DiffKind::TypeChanged),
                (Some(a), Some(b))
                    if a.object_id != b.object_id || a.content_digest != b.content_digest =>
                {
                    Some(DiffKind::ContentChanged)
                }
                (Some(a), Some(b))
                    if a.mode != b.mode || a.atime_ns != b.atime_ns || a.mtime_ns != b.mtime_ns =>
                {
                    Some(DiffKind::MetadataChanged)
                }
                _ => None,
            };
            if let Some(kind) = kind {
                result.push(PathDiff { path, kind });
            }
        }
        Ok(result)
    }

    pub fn text_diff(
        &self,
        left_id: GenerationId,
        right_id: GenerationId,
        max_file_bytes: usize,
    ) -> Result<TextDiffReport> {
        let (_, left) = self.load_generation(left_id)?;
        let (_, right) = self.load_generation(right_id)?;
        let paths: BTreeSet<_> = left
            .entries
            .keys()
            .chain(right.entries.keys())
            .cloned()
            .collect();
        let mut files = Vec::new();
        for path in paths {
            let old = left.entries.get(&path);
            let new = right.entries.get(&path);
            let Some(kind) = classify_diff(old, new) else {
                continue;
            };
            let old_size = old.map(|entry| entry.size);
            let new_size = new.map(|entry| entry.size);
            let old_digest = old.and_then(|entry| entry.content_digest).map(hex::encode);
            let new_digest = new.and_then(|entry| entry.content_digest).map(hex::encode);
            let both_files = old.is_none_or(|entry| entry.kind == EntryKind::File)
                && new.is_none_or(|entry| entry.kind == EntryKind::File)
                && (old.is_some() || new.is_some());
            let (disposition, patch) = if kind == DiffKind::MetadataChanged {
                (TextDiffDisposition::MetadataOnly, None)
            } else if kind == DiffKind::TypeChanged {
                (TextDiffDisposition::TypeChanged, None)
            } else if !both_files {
                (TextDiffDisposition::NonFile, None)
            } else if old_size.unwrap_or(0) > max_file_bytes as u64
                || new_size.unwrap_or(0) > max_file_bytes as u64
            {
                (TextDiffDisposition::TooLarge, None)
            } else {
                let old_bytes = read_entry_data(self, old)?;
                let new_bytes = read_entry_data(self, new)?;
                match (
                    std::str::from_utf8(&old_bytes),
                    std::str::from_utf8(&new_bytes),
                ) {
                    (Ok(old_text), Ok(new_text))
                        if !old_bytes.contains(&0) && !new_bytes.contains(&0) =>
                    {
                        (
                            TextDiffDisposition::Unified,
                            Some(whole_file_patch(
                                &path,
                                old.is_some(),
                                new.is_some(),
                                old_text,
                                new_text,
                            )),
                        )
                    }
                    _ => (TextDiffDisposition::Binary, None),
                }
            };
            files.push(TextPathDiff {
                path,
                kind,
                disposition,
                old_size,
                new_size,
                old_digest,
                new_digest,
                patch,
            });
        }
        Ok(TextDiffReport {
            left: left_id,
            right: right_id,
            files,
        })
    }

    pub fn validate_manifest(manifest: &Manifest) -> Result<()> {
        let root = manifest
            .entries
            .get(&LogicalPath::root())
            .ok_or_else(|| CensorFsError::corrupt("manifest has no root"))?;
        if root.kind != EntryKind::Directory {
            return Err(CensorFsError::corrupt("manifest root is not a directory"));
        }
        for (path, entry) in &manifest.entries {
            if path != &entry.path {
                return Err(CensorFsError::corrupt("manifest key/path mismatch"));
            }
            if !path.is_root() {
                let parent = path.parent().unwrap();
                match manifest.entries.get(&parent) {
                    Some(value) if value.kind == EntryKind::Directory => {}
                    _ => {
                        return Err(CensorFsError::corrupt(format!(
                            "parent of {path} is missing or not a directory"
                        )))
                    }
                }
            }
            match entry.kind {
                EntryKind::File if entry.object_id.is_none() || entry.content_digest.is_none() => {
                    return Err(CensorFsError::corrupt("file entry lacks object"))
                }
                EntryKind::Directory
                    if entry.object_id.is_some() || entry.content_digest.is_some() =>
                {
                    return Err(CensorFsError::corrupt("directory entry references object"))
                }
                _ => {}
            }
        }
        if manifest.entries.len() > 1_000_000 {
            return Err(CensorFsError::new(
                ErrorCode::Unsupported,
                "manifest exceeds v1 entry limit",
            ));
        }
        Ok(())
    }
}

fn classify_diff(old: Option<&ManifestEntry>, new: Option<&ManifestEntry>) -> Option<DiffKind> {
    match (old, new) {
        (None, Some(_)) => Some(DiffKind::Added),
        (Some(_), None) => Some(DiffKind::Deleted),
        (Some(a), Some(b)) if a.kind != b.kind => Some(DiffKind::TypeChanged),
        (Some(a), Some(b))
            if a.object_id != b.object_id || a.content_digest != b.content_digest =>
        {
            Some(DiffKind::ContentChanged)
        }
        (Some(a), Some(b))
            if a.mode != b.mode || a.atime_ns != b.atime_ns || a.mtime_ns != b.mtime_ns =>
        {
            Some(DiffKind::MetadataChanged)
        }
        _ => None,
    }
}

fn read_entry_data(store: &VersionStore, entry: Option<&ManifestEntry>) -> Result<Vec<u8>> {
    match entry {
        None => Ok(Vec::new()),
        Some(entry) if entry.kind == EntryKind::File => store.read_object(
            entry
                .object_id
                .ok_or_else(|| CensorFsError::corrupt("file entry lacks object id"))?,
            entry
                .content_digest
                .ok_or_else(|| CensorFsError::corrupt("file entry lacks content digest"))?,
        ),
        Some(_) => Ok(Vec::new()),
    }
}

fn whole_file_patch(
    path: &LogicalPath,
    old_exists: bool,
    new_exists: bool,
    old: &str,
    new: &str,
) -> String {
    let old_name = if old_exists {
        format!("a/{path}")
    } else {
        "/dev/null".to_string()
    };
    let new_name = if new_exists {
        format!("b/{path}")
    } else {
        "/dev/null".to_string()
    };
    let old_lines: Vec<_> = old.split_terminator('\n').collect();
    let new_lines: Vec<_> = new.split_terminator('\n').collect();
    let old_start = if old_lines.is_empty() { 0 } else { 1 };
    let new_start = if new_lines.is_empty() { 0 } else { 1 };
    let mut patch = format!(
        "--- {old_name}\n+++ {new_name}\n@@ -{old_start},{} +{new_start},{} @@\n",
        old_lines.len(),
        new_lines.len()
    );
    for line in old_lines {
        patch.push('-');
        patch.push_str(line);
        patch.push('\n');
    }
    if !old.is_empty() && !old.ends_with('\n') {
        patch.push_str("\\ No newline at end of file\n");
    }
    for line in new_lines {
        patch.push('+');
        patch.push_str(line);
        patch.push('\n');
    }
    if !new.is_empty() && !new.ends_with('\n') {
        patch.push_str("\\ No newline at end of file\n");
    }
    patch
}
