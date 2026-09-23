//! Crash-recoverable append-only records used between collection and storage.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const MAGIC: [u8; 8] = *b"CSPLv001";
const CHECKPOINT_MAGIC: [u8; 8] = *b"CSPCK001";
const HEADER_BYTES: usize = 32;
const CHECKPOINT_BYTES: usize = 28;
const DEFAULT_SEGMENT_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpoolConfig {
    pub segment_bytes: u64,
    pub max_bytes: u64,
}
impl Default for SpoolConfig {
    fn default() -> Self {
        Self {
            segment_bytes: DEFAULT_SEGMENT_BYTES,
            max_bytes: 4 * 1024 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecordKind {
    Event = 1,
    Payload = 2,
    Control = 3,
    RawEvent = 4,
}
impl TryFrom<u16> for RecordKind {
    type Error = SpoolError;
    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Event),
            2 => Ok(Self::Payload),
            3 => Ok(Self::Control),
            4 => Ok(Self::RawEvent),
            other => Err(SpoolError::InvalidRecord(format!(
                "unknown record kind {other}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SpoolPosition {
    pub segment: u64,
    pub offset: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpoolRecord {
    pub kind: RecordKind,
    pub sequence: u64,
    pub payload: Vec<u8>,
    pub next: SpoolPosition,
}

#[derive(Debug)]
pub enum SpoolError {
    Io(io::Error),
    InvalidRecord(String),
    SequenceOverflow,
    Full {
        limit: u64,
        usage: u64,
        requested: u64,
    },
}
impl std::fmt::Display for SpoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::InvalidRecord(m) => write!(f, "invalid spool record: {m}"),
            Self::SequenceOverflow => f.write_str("spool sequence exhausted"),
            Self::Full {
                limit,
                usage,
                requested,
            } => write!(
                f,
                "spool capacity exceeded: limit={limit} usage={usage} requested={requested}"
            ),
        }
    }
}
impl std::error::Error for SpoolError {}
impl From<io::Error> for SpoolError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct SpoolWriter {
    dir: PathBuf,
    config: SpoolConfig,
    segment: u64,
    next_sequence: u64,
    file: File,
    offset: u64,
}
impl SpoolWriter {
    pub fn open(path: impl AsRef<Path>, config: SpoolConfig) -> Result<Self, SpoolError> {
        let dir = path.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        let mut segments = segment_numbers(&dir)?;
        segments.sort_unstable();
        let segment = segments.last().copied().unwrap_or(0);
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(segment_path(&dir, segment))?;
        let offset = recover_segment(&mut file, segment)?;
        let next_sequence = scan_next_sequence(&dir, &segments, segment)?;
        file.seek(SeekFrom::End(0))?;
        Ok(Self {
            dir,
            config: SpoolConfig {
                segment_bytes: config.segment_bytes.max((HEADER_BYTES + 1) as u64),
                max_bytes: config.max_bytes,
            },
            segment,
            next_sequence,
            file,
            offset,
        })
    }
    pub fn append(
        &mut self,
        kind: RecordKind,
        payload: &[u8],
    ) -> Result<SpoolPosition, SpoolError> {
        let length = u32::try_from(payload.len())
            .map_err(|_| SpoolError::InvalidRecord("payload exceeds u32 length".into()))?;
        let total = HEADER_BYTES as u64 + length as u64;
        if self.config.max_bytes > 0 {
            let usage = spool_usage_bytes(&self.dir)?;
            if usage.saturating_add(total) > self.config.max_bytes {
                return Err(SpoolError::Full {
                    limit: self.config.max_bytes,
                    usage,
                    requested: total,
                });
            }
        }
        if self.offset > 0 && self.offset.saturating_add(total) > self.config.segment_bytes {
            self.file.sync_data()?;
            self.segment = self
                .segment
                .checked_add(1)
                .ok_or(SpoolError::SequenceOverflow)?;
            self.file = OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .open(segment_path(&self.dir, self.segment))?;
            self.offset = 0;
        }
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(SpoolError::SequenceOverflow)?;
        let position = SpoolPosition {
            segment: self.segment,
            offset: self.offset,
        };
        let mut header = [0u8; HEADER_BYTES];
        header[..8].copy_from_slice(&MAGIC);
        header[8..10].copy_from_slice(&(kind as u16).to_le_bytes());
        header[16..24].copy_from_slice(&sequence.to_le_bytes());
        header[24..28].copy_from_slice(&length.to_le_bytes());
        header[28..32].copy_from_slice(&crc32(payload).to_le_bytes());
        self.file.write_all(&header)?;
        self.file.write_all(payload)?;
        self.offset = self.offset.saturating_add(total);
        Ok(position)
    }
    pub fn sync(&mut self) -> Result<(), SpoolError> {
        self.file.sync_data().map_err(SpoolError::Io)
    }
    pub fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    pub fn usage_bytes(&self) -> Result<u64, SpoolError> {
        spool_usage_bytes(&self.dir)
    }
}

pub struct SpoolReader {
    dir: PathBuf,
    position: SpoolPosition,
}
impl SpoolReader {
    pub fn open(path: impl AsRef<Path>, position: SpoolPosition) -> Result<Self, SpoolError> {
        let dir = path.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;
        Ok(Self { dir, position })
    }
    pub fn read_batch(&mut self, limit: usize) -> Result<Vec<SpoolRecord>, SpoolError> {
        let mut out = Vec::new();
        while out.len() < limit {
            let Some(record) = self.read_one()? else {
                break;
            };
            self.position = record.next;
            out.push(record);
        }
        Ok(out)
    }

    pub fn read_next(&mut self) -> Result<Option<SpoolRecord>, SpoolError> {
        let record = self.read_one()?;
        if let Some(record) = &record {
            self.position = record.next;
        }
        Ok(record)
    }
    pub fn position(&self) -> SpoolPosition {
        self.position
    }

    pub fn seek(&mut self, position: SpoolPosition) {
        self.position = position;
    }

    pub fn reclaim_committed(&self, retain_segments: u64) -> Result<usize, SpoolError> {
        self.reclaim_committed_at(self.position, retain_segments)
    }

    pub fn reclaim_committed_at(
        &self,
        position: SpoolPosition,
        retain_segments: u64,
    ) -> Result<usize, SpoolError> {
        let cutoff = position.segment.saturating_sub(retain_segments);
        let mut removed = 0;
        for segment in segment_numbers(&self.dir)? {
            if segment < cutoff {
                match fs::remove_file(segment_path(&self.dir, segment)) {
                    Ok(()) => removed += 1,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        Ok(removed)
    }
    fn read_one(&self) -> Result<Option<SpoolRecord>, SpoolError> {
        let mut file = match File::open(segment_path(&self.dir, self.position.segment)) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        file.seek(SeekFrom::Start(self.position.offset))?;
        let mut header = [0u8; HEADER_BYTES];
        match file.read_exact(&mut header) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                let next_segment = self.position.segment.saturating_add(1);
                if segment_path(&self.dir, next_segment).exists() {
                    let next = SpoolReader {
                        dir: self.dir.clone(),
                        position: SpoolPosition {
                            segment: next_segment,
                            offset: 0,
                        },
                    };
                    return next.read_one();
                }
                return Ok(None);
            }
            Err(e) => return Err(e.into()),
        }
        if header[..8] != MAGIC {
            return Err(SpoolError::InvalidRecord("bad magic".into()));
        }
        let kind = RecordKind::try_from(u16::from_le_bytes([header[8], header[9]]))?;
        let sequence = u64::from_le_bytes(header[16..24].try_into().unwrap());
        let length = u32::from_le_bytes(header[24..28].try_into().unwrap()) as usize;
        let expected = u32::from_le_bytes(header[28..32].try_into().unwrap());
        let mut payload = vec![0u8; length];
        file.read_exact(&mut payload)?;
        if crc32(&payload) != expected {
            return Err(SpoolError::InvalidRecord("CRC mismatch".into()));
        }
        Ok(Some(SpoolRecord {
            kind,
            sequence,
            payload,
            next: SpoolPosition {
                segment: self.position.segment,
                offset: self.position.offset + HEADER_BYTES as u64 + length as u64,
            },
        }))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ConsumeOutcome {
    pub records: usize,
    pub last_sequence: Option<u64>,
}

#[derive(Debug)]
pub enum SpoolConsumeError<E> {
    Spool(SpoolError),
    Apply(E),
}

impl<E: std::fmt::Display> std::fmt::Display for SpoolConsumeError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spool(error) => write!(f, "spool error: {error}"),
            Self::Apply(error) => write!(f, "record apply failed: {error}"),
        }
    }
}

impl<E: std::fmt::Debug + std::fmt::Display> std::error::Error for SpoolConsumeError<E> {}

pub struct SpoolConsumer {
    reader: SpoolReader,
    committed: SpoolPosition,
    checkpoint: SpoolCheckpoint,
    limit: usize,
    retain_segments: u64,
}

impl SpoolConsumer {
    pub fn open(
        spool_path: impl AsRef<Path>,
        checkpoint_path: impl AsRef<Path>,
        limit: usize,
    ) -> Result<Self, SpoolError> {
        let checkpoint = SpoolCheckpoint::new(checkpoint_path);
        let position = checkpoint.load()?.unwrap_or(SpoolPosition {
            segment: 0,
            offset: 0,
        });
        Ok(Self {
            reader: SpoolReader::open(spool_path, position)?,
            committed: position,
            checkpoint,
            limit: limit.max(1),
            retain_segments: 2,
        })
    }

    pub fn position(&self) -> SpoolPosition {
        self.reader.position()
    }

    pub fn seek(&mut self, position: SpoolPosition) {
        self.reader.seek(position);
    }

    pub fn read_next(&mut self) -> Result<Option<SpoolRecord>, SpoolError> {
        self.reader.read_next()
    }

    pub fn checkpoint(&mut self, position: SpoolPosition) -> Result<(), SpoolError> {
        self.checkpoint.store(position)?;
        self.committed = position;
        self.reader
            .reclaim_committed_at(self.committed, self.retain_segments)
            .map(|_| ())
    }

    pub fn consume_once<F, E>(
        &mut self,
        mut apply: F,
    ) -> Result<ConsumeOutcome, SpoolConsumeError<E>>
    where
        F: FnMut(&SpoolRecord) -> Result<(), E>,
    {
        let start = self.reader.position();
        let records = self
            .reader
            .read_batch(self.limit)
            .map_err(SpoolConsumeError::Spool)?;
        let mut cursor = start;
        let mut last_sequence = None;
        for record in &records {
            if let Err(error) = apply(record) {
                self.reader.seek(cursor);
                return Err(SpoolConsumeError::Apply(error));
            }
            if let Err(error) = self.checkpoint(record.next) {
                self.reader.seek(cursor);
                return Err(SpoolConsumeError::Spool(error));
            }
            cursor = record.next;
            last_sequence = Some(record.sequence);
        }
        Ok(ConsumeOutcome {
            records: records.len(),
            last_sequence,
        })
    }
}

pub struct SpoolCheckpoint {
    path: PathBuf,
}

impl SpoolCheckpoint {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn load(&self) -> Result<Option<SpoolPosition>, SpoolError> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if bytes.len() != CHECKPOINT_BYTES || bytes[..8] != CHECKPOINT_MAGIC {
            return Err(SpoolError::InvalidRecord("invalid checkpoint".into()));
        }
        let expected = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
        if crc32(&bytes[..24]) != expected {
            return Err(SpoolError::InvalidRecord("checkpoint CRC mismatch".into()));
        }
        Ok(Some(SpoolPosition {
            segment: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            offset: u64::from_le_bytes(bytes[16..24].try_into().unwrap()),
        }))
    }

    pub fn store(&self, position: SpoolPosition) -> Result<(), SpoolError> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut bytes = [0u8; CHECKPOINT_BYTES];
        bytes[..8].copy_from_slice(&CHECKPOINT_MAGIC);
        bytes[8..16].copy_from_slice(&position.segment.to_le_bytes());
        bytes[16..24].copy_from_slice(&position.offset.to_le_bytes());
        let checksum = crc32(&bytes[..24]);
        bytes[24..28].copy_from_slice(&checksum.to_le_bytes());
        let temporary = self.path.with_extension("checkpoint.tmp");
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, &self.path)?;
        if let Some(parent) = self.path.parent() {
            File::open(parent)?.sync_all()?;
        }
        Ok(())
    }
}

fn segment_path(dir: &Path, segment: u64) -> PathBuf {
    dir.join(format!("segment-{segment:020}.log"))
}
fn segment_numbers(dir: &Path) -> Result<Vec<u64>, SpoolError> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let name = entry?.file_name();
        let name = name.to_string_lossy();
        if let Some(number) = name
            .strip_prefix("segment-")
            .and_then(|v| v.strip_suffix(".log"))
        {
            if let Ok(value) = number.parse() {
                out.push(value);
            }
        }
    }
    Ok(out)
}

fn spool_usage_bytes(dir: &Path) -> Result<u64, SpoolError> {
    let mut total = 0u64;
    for segment in segment_numbers(dir)? {
        total = total.saturating_add(
            fs::metadata(segment_path(dir, segment))
                .map_err(SpoolError::Io)?
                .len(),
        );
    }
    Ok(total)
}
fn recover_segment(file: &mut File, _segment: u64) -> Result<u64, SpoolError> {
    let mut offset = 0u64;
    loop {
        file.seek(SeekFrom::Start(offset))?;
        let mut header = [0u8; HEADER_BYTES];
        match file.read_exact(&mut header) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                file.set_len(offset)?;
                return Ok(offset);
            }
            Err(e) => return Err(e.into()),
        }
        if header[..8] != MAGIC {
            file.set_len(offset)?;
            return Ok(offset);
        }
        let length = u32::from_le_bytes(header[24..28].try_into().unwrap()) as usize;
        let mut payload = vec![0u8; length];
        if file.read_exact(&mut payload).is_err()
            || crc32(&payload) != u32::from_le_bytes(header[28..32].try_into().unwrap())
        {
            file.set_len(offset)?;
            return Ok(offset);
        }
        offset = offset + HEADER_BYTES as u64 + length as u64;
    }
}
fn scan_next_sequence(dir: &Path, segments: &[u64], _active: u64) -> Result<u64, SpoolError> {
    let mut next = 0;
    for &segment in segments {
        let mut file = File::open(segment_path(dir, segment))?;
        let end = file.metadata()?.len();
        let mut offset = 0;
        while offset + HEADER_BYTES as u64 <= end {
            file.seek(SeekFrom::Start(offset))?;
            let mut header = [0u8; HEADER_BYTES];
            file.read_exact(&mut header)?;
            let sequence = u64::from_le_bytes(header[16..24].try_into().unwrap());
            next = next.max(sequence.saturating_add(1));
            let length = u32::from_le_bytes(header[24..28].try_into().unwrap()) as u64;
            offset += HEADER_BYTES as u64 + length;
        }
    }
    Ok(next)
}
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temp(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("storage-spool-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }
    #[test]
    fn appends_and_reads() {
        let dir = temp("read");
        let mut writer = SpoolWriter::open(
            &dir,
            SpoolConfig {
                segment_bytes: 64,
                ..SpoolConfig::default()
            },
        )
        .unwrap();
        writer.append(RecordKind::Event, b"one").unwrap();
        writer.append(RecordKind::Payload, b"two").unwrap();
        writer.sync().unwrap();
        let mut reader = SpoolReader::open(
            &dir,
            SpoolPosition {
                segment: 0,
                offset: 0,
            },
        )
        .unwrap();
        let records = reader.read_batch(8).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].sequence, 0);
        assert_eq!(records[1].payload, b"two");
    }
    #[test]
    fn rotates_and_recovers_tail() {
        let dir = temp("recover");
        let mut writer = SpoolWriter::open(
            &dir,
            SpoolConfig {
                segment_bytes: 40,
                ..SpoolConfig::default()
            },
        )
        .unwrap();
        writer.append(RecordKind::Event, b"first").unwrap();
        writer.append(RecordKind::Event, b"second").unwrap();
        writer.sync().unwrap();
        let path = segment_path(&dir, 1);
        assert!(path.exists());
        let mut file = OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(b"partial").unwrap();
        drop(file);
        let writer = SpoolWriter::open(
            &dir,
            SpoolConfig {
                segment_bytes: 40,
                ..SpoolConfig::default()
            },
        )
        .unwrap();
        assert_eq!(writer.next_sequence(), 2);
    }
    #[test]
    fn detects_corruption() {
        let dir = temp("crc");
        let mut writer = SpoolWriter::open(&dir, SpoolConfig::default()).unwrap();
        writer.append(RecordKind::Event, b"payload").unwrap();
        writer.sync().unwrap();
        let path = segment_path(&dir, 0);
        let mut bytes = fs::read(&path).unwrap();
        *bytes.last_mut().unwrap() ^= 1;
        fs::write(path, bytes).unwrap();
        let result = SpoolReader::open(
            &dir,
            SpoolPosition {
                segment: 0,
                offset: 0,
            },
        )
        .unwrap()
        .read_batch(1);
        assert!(matches!(result, Err(SpoolError::InvalidRecord(_))));
    }

    #[test]
    fn checkpoint_round_trips_and_rejects_corruption() {
        let dir = temp("checkpoint");
        let checkpoint = SpoolCheckpoint::new(dir.join("database.checkpoint"));
        assert_eq!(checkpoint.load().unwrap(), None);
        let position = SpoolPosition {
            segment: 7,
            offset: 4096,
        };
        checkpoint.store(position).unwrap();
        assert_eq!(checkpoint.load().unwrap(), Some(position));
        let mut bytes = fs::read(dir.join("database.checkpoint")).unwrap();
        bytes[16] ^= 1;
        fs::write(dir.join("database.checkpoint"), bytes).unwrap();
        assert!(matches!(
            checkpoint.load(),
            Err(SpoolError::InvalidRecord(_))
        ));
    }

    #[test]
    fn reader_can_return_to_a_committed_position() {
        let dir = temp("seek");
        let mut writer = SpoolWriter::open(&dir, SpoolConfig::default()).unwrap();
        writer.append(RecordKind::Event, b"one").unwrap();
        writer.append(RecordKind::Event, b"two").unwrap();
        writer.sync().unwrap();
        let start = SpoolPosition {
            segment: 0,
            offset: 0,
        };
        let mut reader = SpoolReader::open(&dir, start).unwrap();
        let first = reader.read_batch(1).unwrap();
        reader.seek(start);
        let replay = reader.read_batch(1).unwrap();
        assert_eq!(first, replay);
    }

    #[test]
    fn consumer_advances_checkpoint_only_after_apply() {
        let dir = temp("consumer");
        let mut writer = SpoolWriter::open(&dir, SpoolConfig::default()).unwrap();
        writer.append(RecordKind::Event, b"one").unwrap();
        writer.append(RecordKind::Event, b"two").unwrap();
        writer.sync().unwrap();
        let checkpoint_path = dir.join("database.checkpoint");
        let mut consumer = SpoolConsumer::open(&dir, &checkpoint_path, 2).unwrap();
        let mut attempts = 0;
        let error = consumer.consume_once(|record| {
            attempts += 1;
            if record.payload == b"two" {
                Err("retry")
            } else {
                Ok(())
            }
        });
        assert!(matches!(error, Err(SpoolConsumeError::Apply("retry"))));
        assert_eq!(attempts, 2);
        assert_eq!(
            consumer.position(),
            SpoolPosition {
                segment: 0,
                offset: 35
            }
        );
        let mut replayed = Vec::new();
        let outcome = consumer
            .consume_once(|record| {
                replayed.push(record.payload.clone());
                Ok::<_, ()>(())
            })
            .unwrap();
        assert_eq!(outcome.records, 1);
        assert_eq!(replayed, vec![b"two".to_vec()]);
        let reopened = SpoolConsumer::open(&dir, checkpoint_path, 2).unwrap();
        assert_eq!(reopened.position(), consumer.position());
    }

    #[test]
    fn reclaim_keeps_a_safe_segment_window() {
        let dir = temp("reclaim");
        let mut writer = SpoolWriter::open(
            &dir,
            SpoolConfig {
                segment_bytes: 40,
                ..SpoolConfig::default()
            },
        )
        .unwrap();
        for _ in 0..5 {
            writer.append(RecordKind::Event, b"item").unwrap();
        }
        writer.sync().unwrap();

        let mut reader = SpoolReader::open(
            &dir,
            SpoolPosition {
                segment: 0,
                offset: 0,
            },
        )
        .unwrap();
        let records = reader.read_batch(8).unwrap();
        assert_eq!(records.len(), 5);
        assert_eq!(reader.position().segment, 4);

        assert_eq!(reader.reclaim_committed(2).unwrap(), 2);
        assert!(!segment_path(&dir, 0).exists());
        assert!(!segment_path(&dir, 1).exists());
        assert!(segment_path(&dir, 2).exists());
        assert!(segment_path(&dir, 3).exists());
        assert!(segment_path(&dir, 4).exists());
    }

    #[test]
    fn checkpoint_does_not_rewind_read_cursor() {
        let dir = temp("read-ahead");
        let mut writer = SpoolWriter::open(&dir, SpoolConfig::default()).unwrap();
        writer.append(RecordKind::Event, b"one").unwrap();
        writer.append(RecordKind::Event, b"two").unwrap();
        writer.sync().unwrap();

        let checkpoint_path = dir.join("database.checkpoint");
        let mut consumer = SpoolConsumer::open(&dir, &checkpoint_path, 2).unwrap();
        let first = consumer.read_next().unwrap().unwrap();
        let second = consumer.read_next().unwrap().unwrap();
        consumer.checkpoint(first.next).unwrap();
        assert_eq!(consumer.position(), second.next);
    }

    #[test]
    fn append_rejects_full_spool_and_recovers_after_reclaim() {
        let dir = temp("capacity");
        let config = SpoolConfig {
            segment_bytes: 40,
            max_bytes: 70,
        };
        let mut writer = SpoolWriter::open(&dir, config).unwrap();
        writer.append(RecordKind::Event, b"one").unwrap();
        writer.append(RecordKind::Event, b"two").unwrap();
        assert_eq!(writer.usage_bytes().unwrap(), 70);
        assert!(matches!(
            writer.append(RecordKind::Event, b"three"),
            Err(SpoolError::Full {
                limit: 70,
                usage: 70,
                requested: 37
            })
        ));

        let reader = SpoolReader::open(
            &dir,
            SpoolPosition {
                segment: 1,
                offset: 35,
            },
        )
        .unwrap();
        assert_eq!(reader.reclaim_committed(0).unwrap(), 1);
        writer.append(RecordKind::Event, b"new").unwrap();
        assert_eq!(writer.usage_bytes().unwrap(), 70);
    }
}
