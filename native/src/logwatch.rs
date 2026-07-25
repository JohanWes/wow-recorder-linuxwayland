// SPDX-License-Identifier: GPL-3.0-or-later

//! Incremental, coordinator-polled combat-log file reading.

use std::collections::VecDeque;
use std::fmt;
use std::fs::{self, File, Metadata};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use crate::domain::GameFlavor;
use crate::parser::{ParseFailure, ParseTimeContext, ParsedEvent, event_name, parse_line};

const READ_CHUNK_BYTES: usize = 64 * 1024;
const CHECKPOINT_BYTES: usize = 64;
const MAX_LINE_BYTES: usize = 1024 * 1024;
const MAX_DIAGNOSTICS: usize = 32;
/// A new combat log only appears at session boundaries. Directory mtime makes
/// discovery immediate in the normal case; this interval is the fallback for
/// filesystems whose directory timestamps are coarse or unreliable.
const ACTIVE_FILE_REFRESH_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub enum LogError {
    Io { path: PathBuf, source: io::Error },
    NoActiveLog(PathBuf),
}

impl fmt::Display for LogError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::NoActiveLog(path) => {
                write!(formatter, "no WoWCombatLog*.txt file in {}", path.display())
            }
        }
    }
}

impl std::error::Error for LogError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::NoActiveLog(_) => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiagnosticKind {
    InvalidUtf8,
    MalformedTimestamp,
    MalformedRetainedEvent,
    LineTooLong,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogDiagnostic {
    pub kind: DiagnosticKind,
    pub file: PathBuf,
    pub event_name: Option<String>,
    pub line_number: u64,
    pub byte_offset: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

pub struct LogTailer {
    source: PathBuf,
    path: PathBuf,
    flavor: GameFlavor,
    identity: FileIdentity,
    offset: u64,
    incomplete: Vec<u8>,
    incomplete_offset: u64,
    line_number: u64,
    time_context: ParseTimeContext,
    replay: bool,
    checkpoint: Vec<u8>,
    observed_len: u64,
    observed_modified: Option<SystemTime>,
    source_modified: Option<SystemTime>,
    next_active_refresh: Instant,
    discarding_long_line: bool,
    diagnostics: VecDeque<LogDiagnostic>,
}

impl LogTailer {
    /// Open a configured Logs directory (or a concrete log file) for live use.
    /// Existing bytes are deliberately ignored.
    pub fn open(
        path: PathBuf,
        flavor: GameFlavor,
        time_context: ParseTimeContext,
    ) -> Result<Self, LogError> {
        Self::open_mode(path, flavor, time_context, false)
    }

    /// Open a deterministic fixture or replay at byte zero.
    pub fn open_replay(
        path: PathBuf,
        flavor: GameFlavor,
        time_context: ParseTimeContext,
    ) -> Result<Self, LogError> {
        Self::open_mode(path, flavor, time_context, true)
    }

    fn open_mode(
        source: PathBuf,
        flavor: GameFlavor,
        time_context: ParseTimeContext,
        replay: bool,
    ) -> Result<Self, LogError> {
        let path = active_path(&source)?;
        let metadata = metadata(&path)?;
        let identity = file_identity(&metadata);
        let offset = if replay { 0 } else { metadata.len() };
        let checkpoint = read_checkpoint(&path, offset)?;
        let source_modified = (!source.is_file())
            .then(|| fs::metadata(&source).ok()?.modified().ok())
            .flatten();
        Ok(Self {
            source,
            path,
            flavor,
            identity,
            offset,
            incomplete: Vec::new(),
            incomplete_offset: offset,
            line_number: 0,
            time_context,
            replay,
            checkpoint,
            observed_len: metadata.len(),
            observed_modified: metadata.modified().ok(),
            source_modified,
            next_active_refresh: Instant::now() + ACTIVE_FILE_REFRESH_INTERVAL,
            discarding_long_line: false,
            diagnostics: VecDeque::new(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn set_time_context(&mut self, time_context: ParseTimeContext) {
        self.time_context = time_context;
    }

    pub fn poll(&mut self) -> Result<Vec<ParsedEvent>, LogError> {
        self.refresh_active_file()?;
        let current_metadata = metadata(&self.path)?;
        let current_identity = file_identity(&current_metadata);
        let current_modified = current_metadata.modified().ok();
        let metadata_changed = current_metadata.len() != self.observed_len
            || current_modified != self.observed_modified;
        let reset = current_identity != self.identity
            || current_metadata.len() < self.offset
            || (metadata_changed && !self.checkpoint_matches()?);
        if reset {
            self.reset_for_file(current_identity);
        }

        let available = current_metadata.len().saturating_sub(self.offset);
        self.observed_len = current_metadata.len();
        self.observed_modified = current_modified;
        if available == 0 {
            return Ok(Vec::new());
        }
        let mut file = File::open(&self.path).map_err(|source| self.io_error(source))?;
        file.seek(SeekFrom::Start(self.offset))
            .map_err(|source| self.io_error(source))?;
        let mut remaining = available;
        let mut buffer = vec![0_u8; READ_CHUNK_BYTES];
        let mut events = Vec::new();
        while remaining > 0 {
            let amount = remaining.min(READ_CHUNK_BYTES as u64) as usize;
            let bytes_read = file
                .read(&mut buffer[..amount])
                .map_err(|source| self.io_error(source))?;
            if bytes_read == 0 {
                break;
            }
            let read_start = self.offset;
            self.offset += bytes_read as u64;
            remaining -= bytes_read as u64;
            events.extend(self.consume(buffer[..bytes_read].to_vec(), read_start)?);
        }
        self.checkpoint = read_checkpoint(&self.path, self.offset)?;
        Ok(events)
    }

    pub fn take_diagnostics(&mut self) -> Vec<LogDiagnostic> {
        self.diagnostics.drain(..).collect()
    }

    fn refresh_active_file(&mut self) -> Result<(), LogError> {
        if self.source.is_file() || self.replay {
            return Ok(());
        }
        let source_metadata = metadata(&self.source)?;
        let source_modified = source_metadata.modified().ok();
        let now = Instant::now();
        if source_modified == self.source_modified && now < self.next_active_refresh {
            return Ok(());
        }
        let active = active_path(&self.source)?;
        self.source_modified = source_modified;
        self.next_active_refresh = now + ACTIVE_FILE_REFRESH_INTERVAL;
        if active == self.path {
            return Ok(());
        }
        let metadata = metadata(&active)?;
        self.path = active;
        self.reset_for_file(file_identity(&metadata));
        Ok(())
    }

    fn reset_for_file(&mut self, identity: FileIdentity) {
        self.identity = identity;
        self.offset = 0;
        self.incomplete.clear();
        self.incomplete_offset = 0;
        self.line_number = 0;
        self.discarding_long_line = false;
        self.checkpoint.clear();
        self.observed_len = 0;
        self.observed_modified = None;
    }

    fn checkpoint_matches(&self) -> Result<bool, LogError> {
        if self.checkpoint.is_empty() {
            return Ok(true);
        }
        Ok(read_checkpoint(&self.path, self.offset)? == self.checkpoint)
    }

    fn consume(
        &mut self,
        mut bytes: Vec<u8>,
        mut read_start: u64,
    ) -> Result<Vec<ParsedEvent>, LogError> {
        if self.discarding_long_line {
            let Some(end) = bytes.iter().position(|byte| *byte == b'\n') else {
                self.incomplete_offset = read_start + bytes.len() as u64;
                return Ok(Vec::new());
            };
            self.discarding_long_line = false;
            self.line_number += 1;
            bytes.drain(..=end);
            read_start += end as u64 + 1;
        }
        if self.incomplete.is_empty() {
            self.incomplete_offset = read_start;
        }
        self.incomplete.extend_from_slice(&bytes);
        let mut events = Vec::new();
        let mut consumed = 0;

        while let Some(relative_end) = self.incomplete[consumed..]
            .iter()
            .position(|byte| *byte == b'\n')
        {
            let end = consumed + relative_end;
            let offset = self.incomplete_offset + consumed as u64;
            self.line_number += 1;
            if end - consumed > MAX_LINE_BYTES {
                self.push_diagnostic(LogDiagnostic {
                    kind: DiagnosticKind::LineTooLong,
                    file: self.path.clone(),
                    event_name: None,
                    line_number: self.line_number,
                    byte_offset: offset,
                });
                consumed = end + 1;
                continue;
            }
            let line_end = if self.incomplete.get(end.wrapping_sub(1)) == Some(&b'\r') {
                end - 1
            } else {
                end
            };
            let line = self.incomplete[consumed..line_end].to_vec();
            if !line.is_empty() {
                self.consume_line(&line, offset, &mut events);
            }
            consumed = end + 1;
        }

        if consumed != 0 {
            self.incomplete.drain(..consumed);
            self.incomplete_offset += consumed as u64;
        }
        if self.incomplete.len() > MAX_LINE_BYTES {
            self.push_diagnostic(LogDiagnostic {
                kind: DiagnosticKind::LineTooLong,
                file: self.path.clone(),
                event_name: None,
                line_number: self.line_number + 1,
                byte_offset: self.incomplete_offset,
            });
            self.incomplete.clear();
            self.incomplete_offset = self.offset;
            self.discarding_long_line = true;
        }
        Ok(events)
    }

    fn consume_line(&mut self, bytes: &[u8], offset: u64, events: &mut Vec<ParsedEvent>) {
        if bytes.is_empty() {
            return;
        }
        let line = match std::str::from_utf8(bytes) {
            Ok(line) => line,
            Err(_) => {
                self.push_diagnostic(LogDiagnostic {
                    kind: DiagnosticKind::InvalidUtf8,
                    file: self.path.clone(),
                    event_name: None,
                    line_number: self.line_number,
                    byte_offset: offset,
                });
                return;
            }
        };
        match parse_line(self.flavor.clone(), self.time_context, line) {
            Ok(Some(event)) => events.push(event),
            Ok(None) => {}
            Err(failure) => {
                let kind = match failure {
                    ParseFailure::MalformedTimestamp => DiagnosticKind::MalformedTimestamp,
                    ParseFailure::MalformedRetainedEvent => DiagnosticKind::MalformedRetainedEvent,
                };
                self.push_diagnostic(LogDiagnostic {
                    kind,
                    file: self.path.clone(),
                    event_name: event_name(line).map(|name| name.chars().take(64).collect()),
                    line_number: self.line_number,
                    byte_offset: offset,
                });
            }
        }
    }

    fn push_diagnostic(&mut self, diagnostic: LogDiagnostic) {
        if self.diagnostics.len() == MAX_DIAGNOSTICS {
            self.diagnostics.pop_front();
        }
        self.diagnostics.push_back(diagnostic);
    }

    fn io_error(&self, source: io::Error) -> LogError {
        LogError::Io {
            path: self.path.clone(),
            source,
        }
    }
}

fn active_path(source: &Path) -> Result<PathBuf, LogError> {
    if source.is_file() {
        return Ok(source.to_owned());
    }
    let entries = fs::read_dir(source).map_err(|source_error| LogError::Io {
        path: source.to_owned(),
        source: source_error,
    })?;
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source_error| LogError::Io {
            path: source.to_owned(),
            source: source_error,
        })?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("WoWCombatLog") || !name.ends_with(".txt") {
            continue;
        }
        let metadata = entry.metadata().map_err(|source_error| LogError::Io {
            path: entry.path(),
            source: source_error,
        })?;
        if metadata.is_file() {
            candidates.push((metadata.modified().ok(), name.into_owned(), entry.path()));
        }
    }
    candidates.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    candidates
        .pop()
        .map(|(_, _, path)| path)
        .ok_or_else(|| LogError::NoActiveLog(source.to_owned()))
}

fn metadata(path: &Path) -> Result<Metadata, LogError> {
    fs::metadata(path).map_err(|source| LogError::Io {
        path: path.to_owned(),
        source,
    })
}

fn read_checkpoint(path: &Path, offset: u64) -> Result<Vec<u8>, LogError> {
    let amount = offset.min(CHECKPOINT_BYTES as u64) as usize;
    if amount == 0 {
        return Ok(Vec::new());
    }
    let mut file = File::open(path).map_err(|source| LogError::Io {
        path: path.to_owned(),
        source,
    })?;
    file.seek(SeekFrom::Start(offset - amount as u64))
        .map_err(|source| LogError::Io {
            path: path.to_owned(),
            source,
        })?;
    let mut checkpoint = vec![0; amount];
    file.read_exact(&mut checkpoint)
        .map_err(|source| LogError::Io {
            path: path.to_owned(),
            source,
        })?;
    Ok(checkpoint)
}

#[cfg(unix)]
fn file_identity(metadata: &Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;
    FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[cfg(not(unix))]
fn file_identity(metadata: &Metadata) -> FileIdentity {
    FileIdentity {
        device: 0,
        inode: metadata.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
    const CONTEXT: ParseTimeContext = ParseTimeContext::new(2026, 0);
    const EVENT: &str =
        "4/9 19:27:13.200  ENCOUNTER_START,9999,\"Training Construct\",16,20,777,1\n";

    fn test_directory() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "warcraft-recorder-logwatch-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn retained_event_split_at_every_byte_is_emitted_once() {
        for split in 0..=EVENT.len() {
            let directory = test_directory();
            let path = directory.join("WoWCombatLog.txt");
            fs::write(&path, &EVENT.as_bytes()[..split]).unwrap();
            let mut tailer =
                LogTailer::open_replay(path.clone(), GameFlavor::Retail, CONTEXT).unwrap();
            let mut events = tailer.poll().unwrap();
            let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
            file.write_all(&EVENT.as_bytes()[split..]).unwrap();
            events.extend(tailer.poll().unwrap());
            assert_eq!(events.len(), 1, "split at {split}");
            fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn one_poll_drains_the_available_file_snapshot() {
        let directory = test_directory();
        let path = directory.join("WoWCombatLog.txt");
        let event_count = READ_CHUNK_BYTES / EVENT.len() + 2;
        fs::write(&path, EVENT.repeat(event_count)).unwrap();
        let mut tailer = LogTailer::open_replay(path, GameFlavor::Retail, CONTEXT).unwrap();

        assert_eq!(tailer.poll().unwrap().len(), event_count);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn live_starts_at_eof_replay_starts_at_zero_and_crlf_is_accepted() {
        let directory = test_directory();
        let path = directory.join("WoWCombatLog.txt");
        fs::write(&path, EVENT.replace('\n', "\r\n")).unwrap();
        let mut live = LogTailer::open(path.clone(), GameFlavor::Retail, CONTEXT).unwrap();
        assert!(live.poll().unwrap().is_empty());
        let mut replay = LogTailer::open_replay(path, GameFlavor::Retail, CONTEXT).unwrap();
        assert_eq!(replay.poll().unwrap().len(), 1);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn same_file_truncation_and_new_active_file_restart_at_zero() {
        let directory = test_directory();
        let first = directory.join("WoWCombatLog-1.txt");
        fs::write(&first, vec![b'x'; EVENT.len() * 2]).unwrap();
        let mut tailer = LogTailer::open(directory.clone(), GameFlavor::Retail, CONTEXT).unwrap();
        fs::write(&first, EVENT).unwrap();
        assert_eq!(tailer.poll().unwrap().len(), 1);

        let second = directory.join("WoWCombatLog-2.txt");
        fs::write(&second, EVENT).unwrap();
        assert_eq!(tailer.poll().unwrap().len(), 1);
        assert_eq!(tailer.path(), second);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn live_context_is_explicit_non_utc_and_refreshable() {
        let directory = test_directory();
        let path = directory.join("WoWCombatLog.txt");
        fs::write(&path, "existing").unwrap();
        let mut tailer = LogTailer::open(
            path.clone(),
            GameFlavor::Retail,
            ParseTimeContext::new(2026, 120),
        )
        .unwrap();
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(EVENT.as_bytes()).unwrap();
        let first = tailer.poll().unwrap();
        assert_eq!(first[0].occurred_at_ms, 1_775_755_633_200);

        tailer.set_time_context(ParseTimeContext::new(2027, -300));
        file.write_all(EVENT.as_bytes()).unwrap();
        let second = tailer.poll().unwrap();
        assert_eq!(second[0].occurred_at_ms, 1_807_316_833_200);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn same_inode_truncate_and_regrow_past_offset_restarts_at_zero() {
        let directory = test_directory();
        let path = directory.join("WoWCombatLog.txt");
        let old_event = EVENT.replace("Training Construct", "Retired Construct");
        fs::write(&path, old_event.repeat(2)).unwrap();
        let original_identity = file_identity(&metadata(&path).unwrap());
        let mut tailer = LogTailer::open(path.clone(), GameFlavor::Retail, CONTEXT).unwrap();

        let mut file = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.write_all(EVENT.repeat(3).as_bytes()).unwrap();
        file.flush().unwrap();
        assert_eq!(file_identity(&metadata(&path).unwrap()), original_identity);
        assert_eq!(tailer.poll().unwrap().len(), 3);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn diagnostics_are_bounded_and_never_retain_line_contents() {
        let directory = test_directory();
        let path = directory.join("WoWCombatLog.txt");
        fs::write(
            &path,
            b"4/9 19:27:13.200  UNIT_DIED,secret\n4/9 19:27:13.200  UNIT_DIED,\xff\n",
        )
        .unwrap();
        let mut tailer = LogTailer::open_replay(path.clone(), GameFlavor::Retail, CONTEXT).unwrap();
        assert!(tailer.poll().unwrap().is_empty());
        let diagnostics = tailer.take_diagnostics();
        assert_eq!(diagnostics.len(), 2);
        assert_eq!(
            diagnostics.iter().map(|item| item.kind).collect::<Vec<_>>(),
            [
                DiagnosticKind::MalformedRetainedEvent,
                DiagnosticKind::InvalidUtf8
            ]
        );
        assert_eq!(diagnostics[0].event_name.as_deref(), Some("UNIT_DIED"));

        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        for _ in 0..40 {
            file.write_all(b"4/9 bad  ENCOUNTER_START,private\n")
                .unwrap();
        }
        assert!(tailer.poll().unwrap().is_empty());
        let diagnostics = tailer.take_diagnostics();
        assert_eq!(diagnostics.len(), MAX_DIAGNOSTICS);
        assert!(
            diagnostics
                .iter()
                .all(|item| item.event_name.as_deref() == Some("ENCOUNTER_START"))
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn overlong_line_reports_once_then_recovers_at_newline() {
        let directory = test_directory();
        let path = directory.join("WoWCombatLog.txt");
        let overlong_size = MAX_LINE_BYTES * 2 + 10;
        fs::write(&path, vec![b'x'; overlong_size]).unwrap();
        let mut tailer = LogTailer::open_replay(path.clone(), GameFlavor::Retail, CONTEXT).unwrap();
        for _ in 0..=overlong_size / READ_CHUNK_BYTES + 1 {
            assert!(tailer.poll().unwrap().is_empty());
        }
        assert_eq!(
            tailer
                .take_diagnostics()
                .iter()
                .filter(|item| item.kind == DiagnosticKind::LineTooLong)
                .count(),
            1
        );

        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"\n").unwrap();
        file.write_all(EVENT.as_bytes()).unwrap();
        assert_eq!(tailer.poll().unwrap().len(), 1);
        assert!(tailer.take_diagnostics().is_empty());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn complete_overlong_line_is_rejected_before_parsing_and_next_line_recovers() {
        let directory = test_directory();
        let path = directory.join("WoWCombatLog.txt");
        let mut bytes = vec![b'x'; MAX_LINE_BYTES + 10];
        bytes.push(b'\n');
        bytes.extend_from_slice(EVENT.as_bytes());
        fs::write(&path, bytes).unwrap();
        let mut tailer = LogTailer::open_replay(path, GameFlavor::Retail, CONTEXT).unwrap();
        let mut events = Vec::new();
        for _ in 0..=MAX_LINE_BYTES / READ_CHUNK_BYTES + 1 {
            events.extend(tailer.poll().unwrap());
        }
        assert_eq!(events.len(), 1);
        assert_eq!(
            tailer
                .take_diagnostics()
                .iter()
                .filter(|item| item.kind == DiagnosticKind::LineTooLong)
                .count(),
            1
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
