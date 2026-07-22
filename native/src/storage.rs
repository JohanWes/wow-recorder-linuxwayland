// SPDX-License-Identifier: GPL-3.0-or-later

//! Filesystem-backed recording library.
//!
//! JSON sidecars next to the media are the only source of truth: there is no
//! database, thumbnail cache, or recursive crawl. `Storage` owns the configured
//! recording directory plus the capture root's `replay`/`regular`/`staging`
//! directories, and provides the scan, finalize, mutate, delete, evict, and
//! startup-sweep operations. FFmpeg work lives in `media_jobs`; this module
//! never spawns a process.
//!
//! Reconciliation notes against WR-007:
//! - `finalize` takes the already-combined media produced by the media worker
//!   (`CombinedMedia`) instead of invoking FFmpeg itself, so storage stays
//!   process-free and the worker keeps sole ownership of FFmpeg argv/progress.
//! - `update`, `delete`, `reveal_path`, and `enforce_limit` take the scanned
//!   `LibraryEntry` rather than a bare identifier: the entry already carries its
//!   sidecar/media paths, so no second in-memory index is needed.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::activity::RecordingDraft;
use crate::domain::{
    ActivityDetails, BLOODLUST_DURATION_MS, Category, Codec, CombatantSummary, CorrelatedActivity,
    GameFlavor, LibraryEntry, MediaFacts, Outcome, PlayerSummary, RecordingId, RoundSummary,
    StorageLimit, TimelineItem, TimelineKind, TimelineShape,
};
use crate::parser::{
    CombatEvent, ParseTimeContext, PlayerObservationKind, is_bloodlust_spell, parse_line,
    parse_timestamp,
};
use crate::recorder::CaptureArtifacts;

/// Schema version written into every native sidecar.
pub const SIDECAR_SCHEMA_VERSION: u32 = 1;

/// WR-000 multi-POV correlation tolerance on the activity start time.
const CORRELATION_TOLERANCE_MS: i64 = 60_000;

/// Directory (under the storage root) the startup sweep quarantines
/// unreferenced artifacts into.
pub const RECOVERY_DIR: &str = "Recovery";

const MEDIA_EXTENSION: &str = "mp4";
const SIDECAR_EXTENSION: &str = "json";

/// Media produced by the media worker for a finished capture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CombinedMedia {
    /// Exclusively created temp file holding the final playable media.
    pub temp_media: PathBuf,
    /// Usable replay lead-in actually present at the front of `temp_media`.
    /// Zero for the regular-only fallback.
    pub actual_replay_ms: u64,
    pub facts: MediaFacts,
}

/// One sidecar that could not be turned into an entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedEntry {
    pub sidecar_path: PathBuf,
    pub reason: String,
}

/// Result of a library scan. Holds no file handles or UI objects.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LibraryIndex {
    /// Reverse chronological, matching the baseline library ordering.
    pub entries: Arc<Vec<LibraryEntry>>,
    pub correlations: Arc<Vec<CorrelatedActivity>>,
    /// Entries whose sidecar was written by the legacy application.
    pub legacy_ids: HashSet<RecordingId>,
    pub skipped: Vec<SkippedEntry>,
    /// Bounded summary: how many unrelated/unsupported files were ignored.
    pub ignored_files: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryUpdate {
    Protected(bool),
    /// An empty or whitespace-only tag clears it, matching the baseline.
    Tag(String),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeleteResult {
    pub deleted: Vec<RecordingId>,
    pub failures: Vec<(RecordingId, String)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EvictionResult {
    pub evicted: Vec<RecordingId>,
    pub freed_bytes: u64,
    pub remaining_bytes: u64,
    /// Protected content alone exceeds a positive limit; nothing was deleted
    /// for it.
    pub protected_over_limit: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Quarantined files, by their new location under `Recovery/`.
    pub quarantined: Vec<PathBuf>,
    pub failures: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TimelineEnrichmentReport {
    pub enriched: usize,
    pub failures: Vec<String>,
}

struct LegacyEnrichmentTarget {
    sidecar_path: PathBuf,
    value: Value,
    start_ms: i64,
    end_ms: i64,
    casts: Vec<LegacyBloodlust>,
    covered: bool,
}

pub struct Storage {
    root: PathBuf,
    replay_dir: PathBuf,
    regular_dir: PathBuf,
    staging_dir: PathBuf,
}

impl Storage {
    /// `root` is the configured recording directory; `capture_root` holds GSR's
    /// `replay`, `regular`, and `staging` subdirectories (WR-006).
    pub fn new(root: impl Into<PathBuf>, capture_root: impl AsRef<Path>) -> Self {
        let capture_root = capture_root.as_ref();
        Self {
            root: root.into(),
            replay_dir: capture_root.join("replay"),
            regular_dir: capture_root.join("regular"),
            staging_dir: capture_root.join("staging"),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn staging_dir(&self) -> &Path {
        &self.staging_dir
    }

    /// Create the directories this module writes into.
    pub fn prepare(&self) -> io::Result<()> {
        fs::create_dir_all(&self.root)?;
        fs::create_dir_all(&self.staging_dir)
    }

    /// The path the UI reveals in a file manager.
    pub fn reveal_path(entry: &LibraryEntry) -> &Path {
        &entry.media_path
    }

    /// Add Bloodlust timestamps to legacy sidecars from their original retail
    /// combat log. Each historical log is streamed at most once and only when
    /// an un-enriched recording falls within that log session. The added key is
    /// retained by the Electron reader as unknown metadata and makes future
    /// scans independent of the large source log.
    pub fn enrich_legacy_bloodlust(
        &self,
        retail_log_dirs: &[PathBuf],
        context: ParseTimeContext,
    ) -> TimelineEnrichmentReport {
        self.enrich_legacy_bloodlust_cancellable(retail_log_dirs, context, || false)
    }

    /// Worker-facing variant. Historical logs can be very large, so shutdown
    /// and newly queued recording finalization must be able to stop this
    /// optional one-time maintenance pass between bounded batches of lines.
    pub(crate) fn enrich_legacy_bloodlust_cancellable(
        &self,
        retail_log_dirs: &[PathBuf],
        context: ParseTimeContext,
        mut cancelled: impl FnMut() -> bool,
    ) -> TimelineEnrichmentReport {
        let mut report = TimelineEnrichmentReport::default();
        let mut targets = self.legacy_enrichment_targets(&mut report.failures);
        if targets.is_empty() {
            return report;
        }

        let mut log_paths = Vec::new();
        for directory in retail_log_dirs {
            match historical_log_paths(directory) {
                Ok(found) => log_paths.extend(found),
                Err(error) => report
                    .failures
                    .push(format!("{}: {error}", directory.display())),
            }
        }
        log_paths.sort();
        log_paths.dedup();
        for log_path in log_paths {
            if cancelled() || targets.iter().all(|target| target.covered) {
                break;
            }
            match collect_bloodlust_casts(&log_path, context, &mut targets, &mut cancelled) {
                Ok(true) => break,
                Ok(false) => {}
                Err(error) => report
                    .failures
                    .push(format!("{}: {error}", log_path.display())),
            }
        }

        for target in targets.iter_mut().filter(|target| target.covered) {
            if cancelled() {
                break;
            }
            target.casts.sort_by_key(|cast| cast.timestamp_ms);
            let Value::Object(object) = &mut target.value else {
                continue;
            };
            let encoded = serde_json::to_value(&target.casts).expect("serializable timeline");
            object.insert("bloodlustTimeline".to_owned(), encoded);
            let json = match pretty_json(&target.value) {
                Ok(json) => json,
                Err(error) => {
                    report.failures.push(format!(
                        "{}: could not encode enrichment: {error}",
                        target.sidecar_path.display()
                    ));
                    continue;
                }
            };
            let temp = temp_sibling(&target.sidecar_path);
            match write_atomic(&temp, Some(&target.sidecar_path), json.as_bytes()) {
                Ok(()) => report.enriched += 1,
                Err(error) => report.failures.push(format!(
                    "{}: could not save enrichment: {error}",
                    target.sidecar_path.display()
                )),
            }
        }
        report
    }

    fn legacy_enrichment_targets(&self, failures: &mut Vec<String>) -> Vec<LegacyEnrichmentTarget> {
        let Ok(read_dir) = fs::read_dir(&self.root) else {
            return Vec::new();
        };
        let mut targets = Vec::new();
        for path in read_dir.filter_map(Result::ok).map(|entry| entry.path()) {
            if path.extension().and_then(|value| value.to_str()) != Some(SIDECAR_EXTENSION) {
                continue;
            }
            let result = (|| -> Result<Option<LegacyEnrichmentTarget>, String> {
                let text = fs::read_to_string(&path).map_err(|error| error.to_string())?;
                let value: Value =
                    serde_json::from_str(&text).map_err(|error| error.to_string())?;
                if value.get("schema_version").is_some()
                    || value.get("bloodlustTimeline").is_some()
                    || value.get("category").and_then(Value::as_str) != Some("Mythic+")
                    || !matches!(
                        value.get("flavour").and_then(Value::as_str),
                        None | Some("Retail")
                    )
                {
                    return Ok(None);
                }
                let start_ms = value
                    .get("start")
                    .and_then(Value::as_f64)
                    .filter(|value| value.is_finite())
                    .map(|value| value as i64);
                let duration_ms = value
                    .get("duration")
                    .and_then(Value::as_f64)
                    .filter(|value| value.is_finite() && *value >= 0.0)
                    .map(seconds_to_ms);
                let (Some(start_ms), Some(duration_ms)) = (start_ms, duration_ms) else {
                    return Ok(None);
                };
                Ok(Some(LegacyEnrichmentTarget {
                    sidecar_path: path.clone(),
                    value,
                    start_ms,
                    end_ms: start_ms.saturating_add(duration_ms as i64),
                    casts: Vec::new(),
                    covered: false,
                }))
            })();
            match result {
                Ok(Some(target)) => targets.push(target),
                Ok(None) => {}
                Err(error) => failures.push(format!("{}: {error}", path.display())),
            }
        }
        targets.sort_by_key(|target| target.start_ms);
        targets
    }

    // -----------------------------------------------------------------------
    // Scan
    // -----------------------------------------------------------------------

    /// Read every sidecar at the configured directory level. Unrelated files are
    /// counted, unreadable sidecars are reported, and nothing is repaired.
    pub fn scan(&self) -> LibraryIndex {
        let mut index = LibraryIndex::default();
        let mut starts: Vec<Option<i64>> = Vec::new();
        let mut used_ids: HashSet<RecordingId> = HashSet::new();

        let Ok(read_dir) = fs::read_dir(&self.root) else {
            return index;
        };
        let mut paths: Vec<PathBuf> = read_dir
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
            .map(|entry| entry.path())
            .collect();
        // Deterministic order regardless of directory iteration order.
        paths.sort();

        for path in paths {
            match path.extension().and_then(|value| value.to_str()) {
                Some(SIDECAR_EXTENSION) => {}
                // Media is discovered through its sidecar; the startup sweep
                // deals with anything unreferenced.
                Some(MEDIA_EXTENSION) => continue,
                _ => {
                    index.ignored_files += 1;
                    continue;
                }
            }

            match self.load_sidecar(&path) {
                Ok(loaded) => {
                    let mut entry = loaded.entry;
                    if !used_ids.insert(entry.id.clone()) {
                        // Legacy identifiers derive from the media name, so a
                        // duplicate is possible; keep both addressable.
                        entry.id = entry.id.with_legacy_duplicate_suffix(&path);
                        used_ids.insert(entry.id.clone());
                    }
                    if loaded.legacy {
                        index.legacy_ids.insert(entry.id.clone());
                    }
                    starts.push(loaded.correlation_start_ms);
                    Arc::make_mut(&mut index.entries).push(entry);
                }
                Err(reason) => index.skipped.push(SkippedEntry {
                    sidecar_path: path,
                    reason,
                }),
            }
        }

        // Reverse chronological, ties broken by media path for determinism.
        let mut order: Vec<usize> = (0..index.entries.len()).collect();
        order.sort_by(|left, right| {
            let left_entry = &index.entries[*left];
            let right_entry = &index.entries[*right];
            right_entry
                .start_unix_ms
                .cmp(&left_entry.start_unix_ms)
                .then_with(|| left_entry.media_path.cmp(&right_entry.media_path))
        });
        let entries: Vec<LibraryEntry> = order
            .iter()
            .map(|position| index.entries[*position].clone())
            .collect();
        let starts: Vec<Option<i64>> = order.iter().map(|position| starts[*position]).collect();
        index.correlations = Arc::new(correlate(&entries, &starts));
        index.entries = Arc::new(entries);
        index
    }

    fn load_sidecar(&self, path: &Path) -> Result<LoadedSidecar, String> {
        let text = fs::read_to_string(path).map_err(|error| format!("unreadable: {error}"))?;
        let value: Value =
            serde_json::from_str(&text).map_err(|error| format!("invalid JSON: {error}"))?;

        if value.get("schema_version").is_some() {
            let sidecar: NativeSidecar = serde_json::from_value(value)
                .map_err(|error| format!("invalid native sidecar: {error}"))?;
            if sidecar.schema_version > SIDECAR_SCHEMA_VERSION {
                return Err(format!(
                    "sidecar schema version {} is newer than {SIDECAR_SCHEMA_VERSION}",
                    sidecar.schema_version
                ));
            }
            let media_path = self.root.join(&sidecar.media_file);
            self.check_owned(&media_path)?;
            let has_content = media_has_content(&media_path)?;
            let start = sidecar.start_unix_ms;
            let mut entry = sidecar.into_entry(media_path, path.to_path_buf());
            entry.media.has_content = has_content;
            entry.validate().map_err(|error| error.to_string())?;
            return Ok(LoadedSidecar {
                entry,
                legacy: false,
                correlation_start_ms: Some(start),
            });
        }

        let legacy: LegacySidecar = serde_json::from_value(value)
            .map_err(|error| format!("invalid legacy sidecar: {error}"))?;
        let media_path = path.with_extension(MEDIA_EXTENSION);
        let has_content = media_has_content(&media_path)?;
        let mtime_ms = file_modified_ms(&media_path).unwrap_or(0);
        let correlation_start_ms = legacy.start.map(|start| start as i64);
        let mut entry = legacy.into_entry(media_path, path.to_path_buf(), mtime_ms)?;
        entry.media.has_content = has_content;
        Ok(LoadedSidecar {
            entry,
            legacy: true,
            correlation_start_ms,
        })
    }

    // -----------------------------------------------------------------------
    // Finalization
    // -----------------------------------------------------------------------

    /// Turn a finished draft plus its combined media into a library entry:
    /// write the sidecar temp, rename media then sidecar, and only then remove
    /// the GSR intermediates. A crash between the renames leaves media the
    /// startup sweep quarantines; nothing playable is deleted.
    pub fn finalize(
        &self,
        draft: &RecordingDraft,
        artifacts: &CaptureArtifacts,
        media: &CombinedMedia,
    ) -> io::Result<LibraryEntry> {
        let media_start_ms = artifacts.regular_started_at_ms - media.actual_replay_ms as i64;
        let regular_ms = artifacts
            .regular_stopped_at_ms
            .saturating_sub(artifacts.regular_started_at_ms)
            .max(0) as u64;
        let duration_ms = media.actual_replay_ms + regular_ms;

        let title = draft
            .title
            .clone()
            .unwrap_or_else(|| default_title(&draft.category));
        let stem = unique_stem(&draft.id, draft.started_at_ms, &title);
        let (media_path, sidecar_path) = self.claim_output(&stem)?;

        let entry = LibraryEntry {
            id: draft.id.clone(),
            media_path,
            sidecar_path,
            category: draft.category.clone(),
            flavor: recorded_flavor(&draft.flavor),
            title,
            start_unix_ms: draft.started_at_ms,
            duration_ms,
            outcome: draft.outcome.unwrap_or(Outcome::Unknown),
            protected: false,
            tag: None,
            activity_hash: draft.activity_hash.clone(),
            player: draft.player.clone(),
            combatants: draft.combatants.clone(),
            details: draft.details.clone(),
            timeline: shift_timeline(
                &draft.timeline,
                draft.started_at_ms,
                media_start_ms,
                duration_ms,
            ),
            media: media.facts.clone(),
        };

        self.write_new_entry(&entry, &media.temp_media)?;

        // Both final names exist: the intermediates are now safe to remove.
        if let Some(replay) = artifacts.replay.as_deref() {
            let _ = fs::remove_file(replay);
        }
        let _ = fs::remove_file(&artifacts.regular);

        Ok(entry)
    }

    /// Claim `<stem>.mp4` exclusively (adding a numeric suffix on collision) and
    /// return it with its sidecar path. Identifiers, not titles, provide the
    /// uniqueness; the suffix only guards an exact-name collision.
    pub fn claim_output(&self, stem: &str) -> io::Result<(PathBuf, PathBuf)> {
        for attempt in 0..100u32 {
            let candidate = if attempt == 0 {
                stem.to_owned()
            } else {
                format!("{stem} ({attempt})")
            };
            let media_path = self.root.join(format!("{candidate}.{MEDIA_EXTENSION}"));
            let sidecar_path = self.root.join(format!("{candidate}.{SIDECAR_EXTENSION}"));
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&media_path)
            {
                Ok(_) => return Ok((media_path, sidecar_path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "no free output name",
        ))
    }

    /// Write the sidecar temp, move the media into place, then rename the
    /// sidecar. `source_media` is consumed.
    pub fn write_new_entry(&self, entry: &LibraryEntry, source_media: &Path) -> io::Result<()> {
        entry
            .validate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        let sidecar_temp = temp_sibling(&entry.sidecar_path);
        let json = NativeSidecar::from_entry(entry, &self.root).to_json()?;
        write_atomic(&sidecar_temp, None, json.as_bytes())?;
        move_file(source_media, &entry.media_path)?;
        fs::rename(&sidecar_temp, &entry.sidecar_path)
    }

    // -----------------------------------------------------------------------
    // Mutation and deletion
    // -----------------------------------------------------------------------

    /// Rewrite only the sidecar, atomically. A legacy sidecar keeps its original
    /// schema and unknown fields: only the `protected`/`tag` keys are patched so
    /// the final AppImage can still read it.
    pub fn update(&self, entry: &LibraryEntry, change: &EntryUpdate) -> io::Result<LibraryEntry> {
        self.check_owned(&entry.sidecar_path)
            .map_err(|error| io::Error::new(io::ErrorKind::PermissionDenied, error))?;
        let text = fs::read_to_string(&entry.sidecar_path)?;
        let value: Value = serde_json::from_str(&text)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;

        let mut updated = entry.clone();
        match change {
            EntryUpdate::Protected(protected) => updated.protected = *protected,
            EntryUpdate::Tag(tag) => {
                updated.tag = if tag.trim().is_empty() {
                    None
                } else {
                    Some(tag.clone())
                };
            }
        }

        let json = if value.get("schema_version").is_some() {
            NativeSidecar::from_entry(&updated, &self.root).to_json()?
        } else {
            // The sole sanctioned untyped escape hatch; it never enters the
            // domain model.
            let Value::Object(mut object) = value else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "legacy sidecar is not a JSON object",
                ));
            };
            match change {
                EntryUpdate::Protected(protected) => {
                    object.insert("protected".to_owned(), Value::Bool(*protected));
                }
                EntryUpdate::Tag(_) => match &updated.tag {
                    Some(tag) => {
                        object.insert("tag".to_owned(), Value::String(tag.clone()));
                    }
                    None => {
                        object.remove("tag");
                    }
                },
            }
            pretty_json(&Value::Object(object))?
        };

        let temp = temp_sibling(&entry.sidecar_path);
        write_atomic(&temp, Some(&entry.sidecar_path), json.as_bytes())?;
        Ok(updated)
    }

    /// Remove media plus sidecar for each entry, reporting per-entry failures.
    /// Symlinks and paths outside the root are refused.
    pub fn delete(&self, entries: &[LibraryEntry]) -> DeleteResult {
        let mut result = DeleteResult::default();
        for entry in entries {
            match self.delete_one(entry) {
                Ok(()) => result.deleted.push(entry.id.clone()),
                Err(error) => result.failures.push((entry.id.clone(), error)),
            }
        }
        result
    }

    fn delete_one(&self, entry: &LibraryEntry) -> Result<(), String> {
        self.check_owned(&entry.media_path)?;
        self.check_owned(&entry.sidecar_path)?;
        fs::remove_file(&entry.media_path).map_err(|error| format!("media: {error}"))?;
        match fs::remove_file(&entry.sidecar_path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(format!("sidecar: {error}")),
        }
    }

    fn check_owned(&self, path: &Path) -> Result<(), String> {
        // The library is deliberately flat. Requiring a direct child closes
        // the intermediate-directory symlink race: a leaf swapped to a
        // symlink is itself unlinked by deletion, never followed.
        if path.parent() != Some(self.root.as_path()) {
            return Err(format!(
                "{} is not a direct child of the storage root",
                path.display()
            ));
        }
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!("{} is a symlink", path.display()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Storage limit
    // -----------------------------------------------------------------------

    /// Evict the oldest unprotected recordings until the library fits the limit.
    /// Unlimited returns without eviction; unrecognized files are never touched.
    pub fn enforce_limit(&self, limit: StorageLimit, entries: &[LibraryEntry]) -> EvictionResult {
        let mut result = EvictionResult::default();
        let usage = |entries: &[LibraryEntry]| -> u64 {
            entries
                .iter()
                .map(|entry| self.entry_size(entry))
                .fold(0u64, u64::saturating_add)
        };

        let StorageLimit::Gib(gib) = limit else {
            result.remaining_bytes = usage(entries);
            return result;
        };
        let Some(limit_bytes) = gib.get().checked_mul(1024 * 1024 * 1024) else {
            result.remaining_bytes = usage(entries);
            return result;
        };

        let mut sized: Vec<(&LibraryEntry, u64)> = entries
            .iter()
            .map(|entry| (entry, self.entry_size(entry)))
            .collect();
        let mut used: u64 = sized
            .iter()
            .map(|(_, size)| *size)
            .fold(0u64, u64::saturating_add);
        let protected_bytes: u64 = sized
            .iter()
            .filter(|(entry, _)| entry.protected)
            .map(|(_, size)| *size)
            .fold(0u64, u64::saturating_add);
        result.protected_over_limit = protected_bytes > limit_bytes;

        // Oldest first; the tie-break keeps the order deterministic.
        sized.sort_by(|left, right| {
            left.0
                .start_unix_ms
                .cmp(&right.0.start_unix_ms)
                .then_with(|| left.0.media_path.cmp(&right.0.media_path))
        });

        for (entry, size) in sized {
            if used <= limit_bytes {
                break;
            }
            if entry.protected {
                continue;
            }
            if self.delete_one(entry).is_ok() {
                used = used.saturating_sub(size);
                result.freed_bytes += size;
                result.evicted.push(entry.id.clone());
            }
        }

        result.remaining_bytes = used;
        result
    }

    fn entry_size(&self, entry: &LibraryEntry) -> u64 {
        let media = fs::metadata(&entry.media_path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        let sidecar = fs::metadata(&entry.sidecar_path)
            .map(|meta| meta.len())
            .unwrap_or(0);
        media.saturating_add(sidecar)
    }

    // -----------------------------------------------------------------------
    // Startup sweep
    // -----------------------------------------------------------------------

    /// Quarantine every media/GSR/`.tmp` artifact in the storage, replay,
    /// regular, and staging directories that no sidecar references. Runs once at
    /// startup, before scan and before capture is armed. Nothing is deleted,
    /// repaired, or claimed by name/time proximity.
    pub fn sweep_orphans(&self) -> RecoveryReport {
        let mut report = RecoveryReport::default();
        let referenced = self.referenced_media();
        let recovery_dir = self.root.join(RECOVERY_DIR);

        let directories = [
            (
                self.root.clone(),
                "unreferenced media or interrupted write in the storage folder",
            ),
            (self.replay_dir.clone(), "replay artifact with no recording"),
            (
                self.regular_dir.clone(),
                "regular artifact with no recording",
            ),
            (self.staging_dir.clone(), "media job intermediate"),
        ];

        for (directory, reason) in directories {
            let Ok(read_dir) = fs::read_dir(&directory) else {
                continue;
            };
            let mut paths: Vec<PathBuf> = read_dir
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
                .map(|entry| entry.path())
                .collect();
            paths.sort();

            for path in paths {
                if directory == self.root {
                    let extension = path.extension().and_then(|value| value.to_str());
                    let sweepable = matches!(extension, Some(MEDIA_EXTENSION) | Some("tmp"));
                    if !sweepable || referenced.contains(&path) {
                        continue;
                    }
                }
                match self.quarantine(&recovery_dir, &path, reason) {
                    Ok(moved) => report.quarantined.push(moved),
                    Err(error) => report.failures.push(format!("{}: {error}", path.display())),
                }
            }
        }
        report
    }

    fn referenced_media(&self) -> HashSet<PathBuf> {
        let mut referenced = HashSet::new();
        let Ok(read_dir) = fs::read_dir(&self.root) else {
            return referenced;
        };
        for path in read_dir
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension().and_then(|value| value.to_str()) == Some(SIDECAR_EXTENSION)
            })
        {
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            match value.get("media_file").and_then(Value::as_str) {
                Some(media_file) => {
                    referenced.insert(self.root.join(media_file));
                }
                None => {
                    referenced.insert(path.with_extension(MEDIA_EXTENSION));
                }
            }
        }
        referenced
    }

    fn quarantine(&self, recovery_dir: &Path, path: &Path, reason: &str) -> io::Result<PathBuf> {
        fs::create_dir_all(recovery_dir)?;
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "artifact".to_owned());
        let mut destination = recovery_dir.join(&name);
        let mut attempt = 1;
        while destination.exists() {
            destination = recovery_dir.join(format!("{attempt}-{name}"));
            attempt += 1;
        }
        move_file(path, &destination)?;
        let mut reason_file = File::create(reason_path(&destination))?;
        writeln!(reason_file, "{reason}\noriginal path: {}", path.display())?;
        Ok(destination)
    }
}

struct LoadedSidecar {
    entry: LibraryEntry,
    legacy: bool,
    /// Recorded activity start used for multi-POV correlation; `None` when the
    /// legacy sidecar had no start time and cannot be correlated.
    correlation_start_ms: Option<i64>,
}

// ---------------------------------------------------------------------------
// Native sidecar
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct NativeSidecar {
    schema_version: u32,
    /// Media file name relative to the storage root.
    media_file: String,
    id: RecordingId,
    category: Category,
    flavor: GameFlavor,
    title: String,
    start_unix_ms: i64,
    duration_ms: u64,
    outcome: Outcome,
    protected: bool,
    tag: Option<String>,
    activity_hash: Option<String>,
    player: Option<PlayerSummary>,
    combatants: Vec<CombatantSummary>,
    details: ActivityDetails,
    timeline: Vec<TimelineItem>,
    media: MediaFacts,
}

impl NativeSidecar {
    fn from_entry(entry: &LibraryEntry, root: &Path) -> Self {
        let media_file = entry
            .media_path
            .strip_prefix(root)
            .unwrap_or(&entry.media_path)
            .to_string_lossy()
            .into_owned();
        Self {
            schema_version: SIDECAR_SCHEMA_VERSION,
            media_file,
            id: entry.id.clone(),
            category: entry.category.clone(),
            flavor: entry.flavor.clone(),
            title: entry.title.clone(),
            start_unix_ms: entry.start_unix_ms,
            duration_ms: entry.duration_ms,
            outcome: entry.outcome,
            protected: entry.protected,
            tag: entry.tag.clone(),
            activity_hash: entry.activity_hash.clone(),
            player: entry.player.clone(),
            combatants: entry.combatants.clone(),
            details: entry.details.clone(),
            timeline: entry.timeline.clone(),
            media: entry.media.clone(),
        }
    }

    fn into_entry(self, media_path: PathBuf, sidecar_path: PathBuf) -> LibraryEntry {
        LibraryEntry {
            id: self.id,
            media_path,
            sidecar_path,
            category: self.category,
            flavor: self.flavor,
            title: self.title,
            start_unix_ms: self.start_unix_ms,
            duration_ms: self.duration_ms,
            outcome: self.outcome,
            protected: self.protected,
            tag: self.tag,
            activity_hash: self.activity_hash,
            player: self.player,
            combatants: self.combatants,
            details: self.details,
            timeline: self.timeline,
            media: self.media,
        }
    }

    fn to_json(&self) -> io::Result<String> {
        serde_json::to_value(self)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
            .and_then(|value| pretty_json(&value))
    }
}

// ---------------------------------------------------------------------------
// Legacy sidecar
// ---------------------------------------------------------------------------

/// The real legacy JSON written by the Electron application. Private to
/// storage: it is converted to the clean model and never exposed. Cloud-only
/// fields are simply not read.
#[derive(Debug, Deserialize)]
struct LegacySidecar {
    #[serde(default)]
    category: String,
    #[serde(rename = "parentCategory")]
    parent_category: Option<String>,
    #[serde(default)]
    duration: f64,
    start: Option<f64>,
    #[serde(rename = "clippedAt")]
    clipped_at: Option<f64>,
    #[serde(default)]
    result: bool,
    flavour: Option<String>,
    #[serde(rename = "zoneID")]
    zone_id: Option<u32>,
    #[serde(rename = "zoneName")]
    zone_name: Option<String>,
    #[serde(rename = "encounterID")]
    encounter_id: Option<u32>,
    #[serde(rename = "encounterName")]
    encounter_name: Option<String>,
    #[serde(rename = "difficultyID")]
    difficulty_id: Option<u32>,
    difficulty: Option<String>,
    player: Option<LegacyCombatant>,
    #[serde(rename = "teamMMR")]
    team_mmr: Option<u32>,
    #[serde(default)]
    deaths: Vec<LegacyDeath>,
    #[serde(rename = "upgradeLevel")]
    upgrade_level: Option<u8>,
    #[serde(rename = "mapID")]
    map_id: Option<u32>,
    #[serde(rename = "challengeModeTimeline", default)]
    challenge_mode_timeline: Vec<LegacySegment>,
    #[serde(rename = "bloodlustTimeline", default)]
    bloodlust_timeline: Vec<LegacyBloodlust>,
    #[serde(rename = "soloShuffleTimeline", default)]
    solo_shuffle_timeline: Vec<LegacyRound>,
    /// Pre-cloud keystone level.
    level: Option<u32>,
    #[serde(rename = "keystoneLevel")]
    keystone_level: Option<u32>,
    #[serde(default)]
    protected: bool,
    #[serde(rename = "soloShuffleRoundsWon")]
    solo_shuffle_rounds_won: Option<u8>,
    #[serde(rename = "soloShuffleRoundsPlayed")]
    solo_shuffle_rounds_played: Option<u8>,
    #[serde(default)]
    combatants: Vec<LegacyCombatant>,
    #[serde(default)]
    affixes: Vec<u32>,
    tag: Option<String>,
    #[serde(rename = "uniqueHash")]
    unique_hash: Option<String>,
    #[serde(rename = "bossPercent")]
    boss_percent: Option<u8>,
    encoder: Option<String>,
    /// Recorder FPS; present on newer sidecars only.
    fps: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct LegacyCombatant {
    #[serde(rename = "_GUID")]
    guid: Option<String>,
    #[serde(rename = "_teamID")]
    team_id: Option<i32>,
    #[serde(rename = "_specID")]
    spec_id: Option<u16>,
    #[serde(rename = "_name")]
    name: Option<String>,
    #[serde(rename = "_realm")]
    realm: Option<String>,
    #[serde(rename = "_region")]
    region: Option<String>,
}

#[derive(Debug, Deserialize)]
struct LegacyDeath {
    #[serde(default)]
    name: String,
    /// Seconds from the activity start.
    #[serde(default)]
    timestamp: f64,
    #[serde(default)]
    friendly: bool,
}

#[derive(Debug, Deserialize)]
struct LegacySegment {
    #[serde(rename = "segmentType")]
    segment_type: Option<String>,
    #[serde(rename = "logStart")]
    log_start: Option<String>,
    #[serde(rename = "logEnd")]
    log_end: Option<String>,
    /// Seconds from the activity start.
    timestamp: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct LegacyRound {
    #[serde(default)]
    round: u32,
    /// Seconds from the activity start.
    #[serde(default)]
    timestamp: f64,
    #[serde(default)]
    result: bool,
    duration: Option<f64>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LegacyBloodlust {
    /// Milliseconds since Unix epoch while enriching, converted to a relative
    /// offset before the legacy sidecar enters the domain.
    #[serde(rename = "timestampMs")]
    timestamp_ms: i64,
    #[serde(rename = "spellId")]
    spell_id: u32,
    #[serde(rename = "spellName")]
    spell_name: String,
}

impl LegacySidecar {
    fn into_entry(
        self,
        media_path: PathBuf,
        sidecar_path: PathBuf,
        mtime_ms: i64,
    ) -> Result<LibraryEntry, String> {
        if self.category.trim().is_empty() {
            return Err("missing category".to_owned());
        }
        if !self.duration.is_finite() || self.duration < 0.0 {
            return Err(format!("invalid duration {}", self.duration));
        }
        let category = legacy_category(&self.category);
        let parent = self.parent_category.as_deref().map(legacy_category);
        let duration_ms = (self.duration * 1000.0).round() as u64;
        let start_unix_ms = self
            .clipped_at
            .or(self.start)
            .filter(|value| value.is_finite())
            .map(|value| value as i64)
            .unwrap_or(mtime_ms);
        let outcome = legacy_outcome(parent.as_ref().unwrap_or(&category), self.result);
        let timeline = self.legacy_timeline(duration_ms);
        let media_name = media_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let details = self.legacy_details(&category, parent, &media_name);
        let title = match legacy_title(
            &self.encounter_name,
            &self.legacy_place_name(),
            self.player.as_ref(),
        ) {
            title if title.is_empty() => default_title(&category),
            title => title,
        };

        let entry = LibraryEntry {
            id: RecordingId::from_legacy(
                None,
                Path::new(media_path.file_name().unwrap_or_default()),
            ),
            media_path,
            sidecar_path,
            category,
            flavor: legacy_flavor(self.flavour.as_deref()),
            title,
            start_unix_ms,
            duration_ms,
            outcome,
            protected: self.protected,
            tag: self.tag.filter(|tag| !tag.trim().is_empty()),
            activity_hash: self.unique_hash,
            player: self.player.as_ref().and_then(legacy_player),
            combatants: self.combatants.iter().map(legacy_combatant).collect(),
            details,
            timeline,
            media: MediaFacts {
                fps: self.fps,
                width: None,
                height: None,
                codec: self.encoder.as_deref().and_then(legacy_codec),
                has_content: true,
            },
        };
        entry.validate().map_err(|error| error.to_string())?;
        Ok(entry)
    }

    fn legacy_timeline(&self, duration_ms: u64) -> Vec<TimelineItem> {
        let mut items: Vec<TimelineItem> = Vec::new();
        for death in &self.deaths {
            let start_ms = seconds_to_ms(death.timestamp);
            if start_ms > duration_ms {
                continue;
            }
            items.push(TimelineItem::point(
                TimelineKind::Death,
                start_ms,
                Some(death.name.clone()),
                Some(if death.friendly {
                    Outcome::Loss
                } else {
                    Outcome::Win
                }),
                None,
            ));
        }

        for segment in &self.challenge_mode_timeline {
            let Some(timestamp) = segment.timestamp else {
                continue;
            };
            let start_ms = seconds_to_ms(timestamp);
            if start_ms > duration_ms {
                continue;
            }
            // The legacy segment length is the difference of its ISO log
            // timestamps, exactly as the current renderer computes it.
            let length_ms = match (
                segment.log_start.as_deref().and_then(iso_epoch_ms),
                segment.log_end.as_deref().and_then(iso_epoch_ms),
            ) {
                (Some(from), Some(to)) => to.saturating_sub(from).max(0) as u64,
                _ => 0,
            };
            let kind = match segment.segment_type.as_deref() {
                Some("Boss") => TimelineKind::Encounter,
                Some("Trash") => TimelineKind::Trash,
                Some(other) => TimelineKind::Unknown(other.to_owned()),
                None => TimelineKind::Unknown(String::new()),
            };
            let end_ms = start_ms.saturating_add(length_ms).min(duration_ms);
            if let Ok(item) = TimelineItem::span(kind, start_ms, end_ms, None, None, None) {
                items.push(item);
            }
        }

        if let Some(activity_start_ms) = self.start.filter(|value| value.is_finite()) {
            let activity_start_ms = activity_start_ms as i64;
            for cast in &self.bloodlust_timeline {
                let start_ms = cast.timestamp_ms.saturating_sub(activity_start_ms).max(0) as u64;
                if start_ms > duration_ms {
                    continue;
                }
                let end_ms = start_ms
                    .saturating_add(BLOODLUST_DURATION_MS)
                    .min(duration_ms);
                if let Ok(item) = TimelineItem::span(
                    TimelineKind::Bloodlust,
                    start_ms,
                    end_ms,
                    Some(cast.spell_name.clone()),
                    None,
                    None,
                ) {
                    items.push(item);
                }
            }
        }

        for round in &self.solo_shuffle_timeline {
            let start_ms = seconds_to_ms(round.timestamp);
            if start_ms > duration_ms {
                continue;
            }
            let label = Some(format!("Round {}", round.round));
            let outcome = Some(if round.result {
                Outcome::Win
            } else {
                Outcome::Loss
            });
            let item = match round.duration {
                Some(seconds) => TimelineItem::span(
                    TimelineKind::Round,
                    start_ms,
                    start_ms
                        .saturating_add(seconds_to_ms(seconds))
                        .min(duration_ms),
                    label,
                    outcome,
                    None,
                )
                .ok(),
                None => Some(TimelineItem::point(
                    TimelineKind::Round,
                    start_ms,
                    label,
                    outcome,
                    None,
                )),
            };
            items.extend(item);
        }

        items.sort_by_key(TimelineItem::start_ms);
        items
    }

    /// Legacy sidecars usually store only `zoneID`/`mapID`; the legacy frontend
    /// resolved the display name from `instanceNamesByZoneId` at render time.
    /// Restore it the same way when the sidecar carries no name.
    fn legacy_place_name(&self) -> Option<String> {
        self.zone_name.clone().or_else(|| {
            crate::activity::instance_name(
                &legacy_flavor(self.flavour.as_deref()),
                self.zone_id.unwrap_or(0),
                self.map_id.unwrap_or(0),
            )
        })
    }

    fn legacy_details(
        &self,
        category: &Category,
        parent: Option<Category>,
        media_name: &str,
    ) -> ActivityDetails {
        let source = match category {
            Category::Clip => {
                return ActivityDetails::Clip {
                    source_recording: legacy_clip_source_id(media_name),
                    source_category: parent.unwrap_or_else(|| Category::Unknown(String::new())),
                    source_title: self
                        .encounter_name
                        .clone()
                        .or_else(|| self.legacy_place_name()),
                };
            }
            other => other,
        };

        match source {
            Category::Raids => ActivityDetails::Raid {
                zone_id: self.zone_id,
                zone_name: self.zone_name.clone(),
                encounter_id: self.encounter_id,
                encounter_name: self.encounter_name.clone(),
                difficulty_id: self.difficulty_id,
                difficulty: self.difficulty.clone(),
                pull: None,
                boss_percent: self.boss_percent,
            },
            Category::MythicPlus => ActivityDetails::Dungeon {
                zone_id: self.zone_id,
                dungeon_name: self.legacy_place_name(),
                map_id: self.map_id,
                keystone_level: self.keystone_level.or(self.level),
                affixes: self.affixes.clone(),
                upgrade_level: self.upgrade_level,
            },
            Category::SoloShuffle => ActivityDetails::SoloRounds {
                map_id: self.zone_id,
                map_name: self.legacy_place_name(),
                rounds_won: self.solo_shuffle_rounds_won,
                rounds_played: self.solo_shuffle_rounds_played,
                rounds: self
                    .solo_shuffle_timeline
                    .iter()
                    .map(|round| RoundSummary {
                        round: round.round,
                        outcome: if round.result {
                            Outcome::Win
                        } else {
                            Outcome::Loss
                        },
                        start_ms: seconds_to_ms(round.timestamp),
                        duration_ms: round.duration.map(seconds_to_ms),
                    })
                    .collect(),
            },
            Category::TwoVTwo
            | Category::ThreeVThree
            | Category::FiveVFive
            | Category::Skirmish
            | Category::Battlegrounds => ActivityDetails::ArenaOrBattleground {
                map_id: self.zone_id,
                map_name: self.legacy_place_name(),
                team_mmr: self.team_mmr,
            },
            Category::Manual => ActivityDetails::Manual,
            Category::Clip | Category::Unknown(_) => ActivityDetails::UnknownLegacy {
                description: self.zone_name.clone(),
            },
        }
    }
}

/// A legacy clip stores no parent identifier, but its filename is the source
/// video name plus ` - Clipped at <date>`. Stripping that suffix rebuilds the
/// identifier the source recording has in this library.
fn legacy_clip_source_id(media_name: &str) -> RecordingId {
    let stem = media_name
        .rsplit_once(" - Clipped at ")
        .map(|(source, _)| format!("{source}.{MEDIA_EXTENSION}"))
        .unwrap_or_else(|| media_name.to_owned());
    RecordingId::from_legacy(None, Path::new(&stem))
}

fn legacy_category(value: &str) -> Category {
    match value {
        "2v2" => Category::TwoVTwo,
        "3v3" => Category::ThreeVThree,
        "5v5" => Category::FiveVFive,
        "Skirmish" => Category::Skirmish,
        "Solo Shuffle" => Category::SoloShuffle,
        "Mythic+" => Category::MythicPlus,
        "Raids" => Category::Raids,
        "Battlegrounds" => Category::Battlegrounds,
        "Clips" => Category::Clip,
        "Manual" => Category::Manual,
        other => Category::Unknown(other.to_owned()),
    }
}

/// The legacy `encoder` string is an OBS encoder id on Windows and the GSR
/// codec on Linux; only the codec family is recoverable from it.
fn legacy_codec(encoder: &str) -> Option<Codec> {
    let encoder = encoder.to_ascii_lowercase();
    if encoder.contains("av1") {
        Some(Codec::Av1)
    } else if encoder.contains("hevc") || encoder.contains("265") {
        Some(Codec::Hevc)
    } else if encoder.contains("264") {
        Some(Codec::H264)
    } else {
        None
    }
}

fn legacy_flavor(value: Option<&str>) -> GameFlavor {
    match value {
        Some("Retail") | None => GameFlavor::Retail,
        Some("Classic") => GameFlavor::Classic,
        Some(other) => GameFlavor::Unknown(other.to_owned()),
    }
}

/// The legacy `result` boolean means different things per category; this is the
/// same mapping the activity engine writes for new recordings.
fn legacy_outcome(category: &Category, result: bool) -> Outcome {
    match category {
        Category::MythicPlus => {
            if result {
                Outcome::Complete
            } else {
                Outcome::Abandoned
            }
        }
        Category::Manual | Category::Unknown(_) => Outcome::Unknown,
        _ => {
            if result {
                Outcome::Win
            } else {
                Outcome::Loss
            }
        }
    }
}

/// Legacy sidecars store no title; the library rebuilds a display string from
/// the fields they do store. Missing values simply drop out.
fn legacy_title(
    encounter_name: &Option<String>,
    zone_name: &Option<String>,
    player: Option<&LegacyCombatant>,
) -> String {
    let base = encounter_name
        .clone()
        .or_else(|| zone_name.clone())
        .unwrap_or_default();
    match player.and_then(|player| player.name.clone()) {
        Some(name) if !base.is_empty() => format!("{name} - {base}"),
        Some(name) => name,
        None => base,
    }
}

fn legacy_player(combatant: &LegacyCombatant) -> Option<PlayerSummary> {
    Some(PlayerSummary {
        name: combatant.name.clone()?,
        realm: combatant.realm.clone(),
        guid: combatant.guid.clone(),
        class_id: None,
        spec_id: combatant.spec_id,
    })
}

fn legacy_combatant(combatant: &LegacyCombatant) -> CombatantSummary {
    CombatantSummary {
        name: combatant.name.clone(),
        realm: combatant.realm.clone(),
        guid: combatant.guid.clone(),
        region: combatant.region.clone(),
        class_id: None,
        spec_id: combatant.spec_id,
        team_id: combatant.team_id.and_then(|team| u8::try_from(team).ok()),
    }
}

// ---------------------------------------------------------------------------
// Correlation
// ---------------------------------------------------------------------------

/// WR-000 correlation: identical unique hash and activity start times within one
/// minute. Clips, solo shuffle, and manual recordings only ever group with the
/// literally identical video, which for a local-only library means never.
fn correlate(entries: &[LibraryEntry], starts: &[Option<i64>]) -> Vec<CorrelatedActivity> {
    let mut correlated: Vec<CorrelatedActivity> = Vec::new();
    let mut primary_starts: Vec<i64> = Vec::new();
    let mut primary_categories: Vec<&Category> = Vec::new();
    let mut primary_hashes: Vec<Option<&str>> = Vec::new();

    for (entry, start) in entries.iter().zip(starts.iter()) {
        let correlatable = entry.activity_hash.as_ref().zip(*start);
        let Some((hash, start)) = correlatable else {
            correlated.push(CorrelatedActivity {
                primary_id: entry.id.clone(),
                local_pov_ids: Vec::new(),
            });
            primary_starts.push(entry.start_unix_ms);
            primary_categories.push(&entry.category);
            primary_hashes.push(entry.activity_hash.as_deref());
            continue;
        };

        let matched = if excluded_from_correlation(&entry.category) {
            None
        } else {
            correlated.iter().enumerate().position(|(position, _)| {
                !excluded_from_correlation(primary_categories[position])
                    && primary_hashes[position] == Some(hash.as_str())
            })
        };

        match matched {
            Some(position)
                if (primary_starts[position] - start).abs() <= CORRELATION_TOLERANCE_MS =>
            {
                correlated[position].local_pov_ids.push(entry.id.clone());
            }
            _ => {
                correlated.push(CorrelatedActivity {
                    primary_id: entry.id.clone(),
                    local_pov_ids: Vec::new(),
                });
                primary_starts.push(start);
                primary_categories.push(&entry.category);
                primary_hashes.push(entry.activity_hash.as_deref());
            }
        }
    }
    correlated
}

fn excluded_from_correlation(category: &Category) -> bool {
    matches!(
        category,
        Category::Clip | Category::SoloShuffle | Category::Manual
    )
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn historical_log_paths(source: &Path) -> io::Result<Vec<PathBuf>> {
    if source.is_file() {
        return Ok(vec![source.to_owned()]);
    }
    let mut paths = Vec::new();
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("WoWCombatLog") && name.ends_with(".txt") {
            paths.push(entry.path());
        }
    }
    Ok(paths)
}

fn collect_bloodlust_casts(
    path: &Path,
    context: ParseTimeContext,
    targets: &mut [LegacyEnrichmentTarget],
    cancelled: &mut impl FnMut() -> bool,
) -> io::Result<bool> {
    let naive_context = ParseTimeContext::new(context.year, 0);
    let mut reader = BufReader::new(File::open(path)?);
    let mut line = String::new();
    let mut starts = Vec::new();
    let mut casts = Vec::new();
    let mut lines = 0usize;
    loop {
        if lines.is_multiple_of(1024) && cancelled() {
            return Ok(true);
        }
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        lines += 1;
        if !line.contains("CHALLENGE_MODE_START") && !line.contains("SPELL_CAST_SUCCESS") {
            continue;
        }
        let Ok(Some(event)) = parse_line(GameFlavor::Retail, naive_context, line.trim_end()) else {
            continue;
        };
        match event.event {
            CombatEvent::ChallengeStarted { .. } => starts.push(event.occurred_at_ms),
            CombatEvent::PlayerObserved {
                kind: PlayerObservationKind::CastSucceeded,
                spell_id,
                spell_name,
                ..
            } if is_bloodlust_spell(spell_id) => {
                casts.push((event.occurred_at_ms, spell_id, spell_name));
            }
            _ => {}
        }
    }
    let Some(last_naive_ms) = last_log_timestamp(path, naive_context)? else {
        return Ok(false);
    };

    for target in targets.iter_mut().filter(|target| !target.covered) {
        // A valid start can only be within two hours of the configured local
        // UTC offset (and two seconds of the rounded timestamp).  Restricting
        // the search to that tiny, time-ordered window avoids walking every
        // historical dungeon start for every legacy sidecar.
        let expected_offset_ms = i64::from(context.utc_offset_minutes) * 60 * 1_000;
        let window_ms = 2 * 60 * 60 * 1_000 + 2_000;
        let first_start = target
            .start_ms
            .saturating_add(expected_offset_ms)
            .saturating_sub(window_ms);
        let last_start = target
            .start_ms
            .saturating_add(expected_offset_ms)
            .saturating_add(window_ms);
        let starts_start = starts.partition_point(|start| *start < first_start);
        let starts_end = starts.partition_point(|start| *start <= last_start);
        let Some(offset_ms) = starts
            .get(starts_start..starts_end)
            .unwrap_or_default()
            .iter()
            .filter_map(|start| {
                rounded_timezone_offset(*start - target.start_ms, context.utc_offset_minutes)
            })
            .min_by_key(|offset| {
                (offset / (60 * 1_000) - i64::from(context.utc_offset_minutes)).abs()
            })
        else {
            continue;
        };
        if last_naive_ms.saturating_sub(offset_ms) < target.end_ms {
            continue;
        }
        target.covered = true;
        let first_cast = target.start_ms.saturating_add(offset_ms);
        let last_cast = target.end_ms.saturating_add(offset_ms);
        let casts_start = casts.partition_point(|(cast_ms, _, _)| *cast_ms < first_cast);
        let casts_end = casts.partition_point(|(cast_ms, _, _)| *cast_ms < last_cast);
        for (cast_naive_ms, spell_id, spell_name) in
            casts.get(casts_start..casts_end).unwrap_or_default()
        {
            let cast_ms = cast_naive_ms.saturating_sub(offset_ms);
            if !target
                .casts
                .iter()
                .any(|cast| cast.timestamp_ms == cast_ms && cast.spell_id == *spell_id)
            {
                target.casts.push(LegacyBloodlust {
                    timestamp_ms: cast_ms,
                    spell_id: *spell_id,
                    spell_name: spell_name.clone(),
                });
            }
        }
    }
    Ok(false)
}

fn rounded_timezone_offset(delta_ms: i64, expected_offset_minutes: i32) -> Option<i64> {
    const QUARTER_HOUR_MS: i64 = 15 * 60 * 1_000;
    const MIN_OFFSET_MS: i64 = -12 * 60 * 60 * 1_000;
    const MAX_OFFSET_MS: i64 = 14 * 60 * 60 * 1_000;
    let rounded =
        ((delta_ms as f64 / QUARTER_HOUR_MS as f64).round() as i64).saturating_mul(QUARTER_HOUR_MS);
    let expected = i64::from(expected_offset_minutes) * 60 * 1_000;
    (MIN_OFFSET_MS..=MAX_OFFSET_MS)
        .contains(&rounded)
        .then_some(rounded)
        .filter(|rounded| delta_ms.saturating_sub(*rounded).abs() <= 2_000)
        // Historical daylight-saving changes stay close to the configured
        // local offset; a larger difference means this is another run's start.
        .filter(|rounded| rounded.saturating_sub(expected).abs() <= 2 * 60 * 60 * 1_000)
}

fn last_log_timestamp(path: &Path, context: ParseTimeContext) -> io::Result<Option<i64>> {
    const TAIL_BYTES: u64 = 1024 * 1024;
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(TAIL_BYTES)))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    Ok(text.lines().rev().find_map(|line| {
        let timestamp = line.split_once("  ")?.0;
        parse_timestamp(timestamp, context).ok()
    }))
}

/// Convert timeline offsets relative to the activity start into media offsets.
/// Markers that fall before the media starts are clipped away.
fn shift_timeline(
    timeline: &[TimelineItem],
    activity_start_ms: i64,
    media_start_ms: i64,
    duration_ms: u64,
) -> Vec<TimelineItem> {
    let lead_in_ms = activity_start_ms - media_start_ms;
    let mut shifted = Vec::new();
    for item in timeline {
        let start = item.start_ms() as i64 + lead_in_ms;
        let end = item.end_ms().map(|end| end as i64 + lead_in_ms);
        if start > duration_ms as i64 {
            continue;
        }
        match item.shape() {
            TimelineShape::Point => {
                if start < 0 {
                    continue;
                }
                shifted.push(TimelineItem::point(
                    item.kind().clone(),
                    start as u64,
                    item.label().map(str::to_owned),
                    item.outcome(),
                    item.player_reference().map(str::to_owned),
                ));
            }
            TimelineShape::Span => {
                let end = end.unwrap_or(start);
                if end < 0 {
                    continue;
                }
                let clamped_end = (end as u64).min(duration_ms);
                if let Ok(span) = TimelineItem::span(
                    item.kind().clone(),
                    (start.max(0) as u64).min(clamped_end),
                    clamped_end,
                    item.label().map(str::to_owned),
                    item.outcome(),
                    item.player_reference().map(str::to_owned),
                ) {
                    shifted.push(span);
                }
            }
        }
    }
    shifted
}

/// Baseline filename sanitizer: invalid characters become spaces and runs of
/// spaces collapse.
pub fn sanitize_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut last_space = false;
    for character in name.chars() {
        let character = match character {
            '<' | '>' | ':' | '"' | '/' | '|' | '?' | '*' | '\\' => ' ',
            other if other.is_control() => ' ',
            other => other,
        };
        if character == ' ' {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(character);
            last_space = false;
        }
    }
    out.trim().to_owned()
}

/// `activity-<start>-<identifier> - <sanitized title>`: uniqueness comes from
/// the identifier, never from the title.
pub fn unique_stem(id: &RecordingId, start_unix_ms: i64, title: &str) -> String {
    let short: String = id
        .as_str()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect();
    let stem = format!("activity-{start_unix_ms}-{short}");
    let title = sanitize_name(title);
    if title.is_empty() {
        stem
    } else {
        format!("{stem} - {title}")
    }
}

fn default_title(category: &Category) -> String {
    match category {
        Category::Manual => "Manual recording".to_owned(),
        Category::Clip => "Clip".to_owned(),
        other => format!("{other:?}"),
    }
}

/// Era log sources record `Classic` in their metadata, matching the legacy
/// `EraLogHandler`.
fn recorded_flavor(flavor: &GameFlavor) -> GameFlavor {
    match flavor {
        GameFlavor::Era => GameFlavor::Classic,
        other => other.clone(),
    }
}

fn seconds_to_ms(seconds: f64) -> u64 {
    if !seconds.is_finite() || seconds <= 0.0 {
        return 0;
    }
    (seconds * 1000.0).round() as u64
}

fn media_has_content(path: &Path) -> Result<bool, String> {
    match fs::metadata(path) {
        // WR-015's deterministic library corpus deliberately uses zero-byte
        // placeholders because scanning must not decode media. Real outputs
        // are still checked as nonzero by the media worker before finalizing.
        Ok(metadata) if metadata.is_file() => Ok(metadata.len() > 0),
        Ok(_) => Err(format!("media path {} is not a file", path.display())),
        Err(error) => Err(format!("media file {}: {error}", path.display())),
    }
}

fn file_modified_ms(path: &Path) -> Option<i64> {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_millis() as i64)
}

fn temp_sibling(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".tmp");
    path.with_file_name(name)
}

fn reason_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".recovery.txt");
    path.with_file_name(name)
}

/// Legacy sidecars are `JSON.stringify(metadata, null, 2)` with no trailing
/// newline; native sidecars use the same shape.
fn pretty_json(value: &Value) -> io::Result<String> {
    serde_json::to_string_pretty(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
}

fn write_atomic(temp: &Path, final_path: Option<&Path>, bytes: &[u8]) -> io::Result<()> {
    let mut file = File::create(temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    match final_path {
        Some(final_path) => fs::rename(temp, final_path),
        None => Ok(()),
    }
}

/// Rename, falling back to copy plus remove across filesystems.
fn move_file(from: &Path, to: &Path) -> io::Result<()> {
    match fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::copy(from, to)?;
            fs::remove_file(from)
        }
    }
}

/// Minimal ISO-8601 UTC parser for the legacy `logStart`/`logEnd` strings
/// (`YYYY-MM-DDTHH:MM:SS[.mmm]Z`). Only their difference is used.
fn iso_epoch_ms(value: &str) -> Option<i64> {
    let number = |from: usize, to: usize| value.get(from..to)?.parse::<i64>().ok();
    let year = number(0, 4)?;
    let month = number(5, 7)?;
    let day = number(8, 10)?;
    let hour = number(11, 13)?;
    let minute = number(14, 16)?;
    let second = number(17, 19)?;
    let millis = value
        .get(20..23)
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0);
    let days = days_from_civil(year, month, day);
    Some((((days * 24 + hour) * 60 + minute) * 60 + second) * 1000 + millis)
}

/// Howard Hinnant's days-from-civil algorithm.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Wall-clock milliseconds; used for generated clip dates.
pub fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroU64;

    fn fixture_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/native/fixtures/legacy/sidecars")
    }

    fn golden_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/native/golden")
            .join(name)
    }

    #[test]
    fn enriches_legacy_bloodlust_from_cast_success_once() {
        let tree = TempTree::new("bloodlust-enrichment");
        let logs = tree.root.join("logs");
        fs::create_dir_all(&logs).unwrap();
        let sidecar = tree.library().join("run.json");
        fs::write(tree.library().join("run.mp4"), b"media").unwrap();
        fs::write(
            &sidecar,
            r#"{
  "category": "Mythic+",
  "duration": 120,
  "start": 1784396963000,
  "result": true,
  "unknownLegacyKey": "preserved"
}"#,
        )
        .unwrap();
        let log = logs.join("WoWCombatLog-071826_191439.txt");
        fs::write(
            log,
            concat!(
                "7/18/2026 19:49:23.0000  CHALLENGE_MODE_START,\"Algeth'ar Academy\",2526,402,22,[9,6,3]\n",
                "7/18/2026 19:50:00.0000  SPELL_AURA_APPLIED,Player-1-A,\"Evoker-Realm\",0x512,0x0,Player-1-B,\"Player-Realm\",0x511,0x0,390386,\"Fury of the Aspects\",0x40,BUFF\n",
                "7/18/2026 19:50:00.0000  SPELL_CAST_SUCCESS,Player-1-A,\"Evoker-Realm\",0x512,0x0,0000000000000000,nil,0x80000000,0x80000000,390386,\"Fury of the Aspects\",0x40\n",
                "7/18/2026 19:50:00.0000  SPELL_CAST_SUCCESS,Player-1-A,\"Evoker-Realm\",0x512,0x0,0000000000000000,nil,0x80000000,0x80000000,390386,\"Fury of the Aspects\",0x40\n",
                "7/18/2026 19:51:24.0000  CHALLENGE_MODE_END,2526,1,3,120000,0,0\n",
            ),
        )
        .unwrap();

        let storage = tree.storage();
        let context = ParseTimeContext::new(2026, 120);
        let report = storage.enrich_legacy_bloodlust(std::slice::from_ref(&logs), context);
        assert_eq!(report.enriched, 1);
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(
            storage
                .enrich_legacy_bloodlust(std::slice::from_ref(&logs), context)
                .enriched,
            0
        );

        let value: Value = serde_json::from_str(&fs::read_to_string(&sidecar).unwrap()).unwrap();
        assert_eq!(value["unknownLegacyKey"], "preserved");
        assert_eq!(value["bloodlustTimeline"].as_array().unwrap().len(), 1);
        let index = storage.scan();
        let marker = index.entries[0]
            .timeline
            .iter()
            .find(|item| item.kind() == &TimelineKind::Bloodlust)
            .unwrap();
        assert_eq!(marker.start_ms(), 37_000);
        assert_eq!(marker.end_ms(), Some(77_000));
        assert_eq!(marker.label(), Some("Fury of the Aspects"));
    }

    #[test]
    fn legacy_enrichment_checks_cancellation_while_streaming_historical_logs() {
        let tree = TempTree::new("bloodlust-cancel");
        let logs = tree.root.join("logs");
        fs::create_dir_all(&logs).unwrap();
        let sidecar = tree.write(
            "run.json",
            r#"{"category":"Mythic+","duration":120,"start":1784396963000}"#,
        );
        tree.write("run.mp4", "media");
        fs::write(logs.join("WoWCombatLog.txt"), "irrelevant\n".repeat(2048)).unwrap();

        let mut checks = 0;
        let report = tree.storage().enrich_legacy_bloodlust_cancellable(
            std::slice::from_ref(&logs),
            ParseTimeContext::new(2026, 120),
            || {
                checks += 1;
                checks >= 3
            },
        );

        assert_eq!(report.enriched, 0);
        assert_eq!(checks, 3);
        let value: Value = serde_json::from_str(&fs::read_to_string(sidecar).unwrap()).unwrap();
        assert!(value.get("bloodlustTimeline").is_none());
    }

    #[test]
    fn legacy_enrichment_cancellation_prevents_persisting_covered_targets() {
        let tree = TempTree::new("bloodlust-cancel-covered");
        let logs = tree.root.join("logs");
        fs::create_dir_all(&logs).unwrap();
        let sidecar = tree.write(
            "run.json",
            r#"{"category":"Mythic+","duration":120,"start":1784396963000}"#,
        );
        tree.write("run.mp4", "media");
        fs::write(
            logs.join("WoWCombatLog.txt"),
            concat!(
                "7/18/2026 19:49:23.0000  CHALLENGE_MODE_START,\"Dungeon\",2526,402,22,[]\n",
                "7/18/2026 19:50:00.0000  SPELL_CAST_SUCCESS,Player-1-A,\"Evoker-Realm\",0x512,0x0,0000000000000000,nil,0x80000000,0x80000000,390386,\"Fury of the Aspects\",0x40\n",
                "7/18/2026 19:51:24.0000  CHALLENGE_MODE_END,2526,1,3,120000,0,0\n",
            ),
        )
        .unwrap();

        let mut checks = 0;
        let report = tree.storage().enrich_legacy_bloodlust_cancellable(
            std::slice::from_ref(&logs),
            ParseTimeContext::new(2026, 120),
            || {
                checks += 1;
                checks >= 3
            },
        );

        assert_eq!(checks, 3, "cancel after the log has covered the target");
        assert_eq!(report.enriched, 0);
        let value: Value = serde_json::from_str(&fs::read_to_string(sidecar).unwrap()).unwrap();
        assert!(value.get("bloodlustTimeline").is_none());
    }

    #[test]
    fn incomplete_log_does_not_cache_a_false_empty_timeline() {
        let tree = TempTree::new("bloodlust-incomplete-log");
        let logs = tree.root.join("logs");
        fs::create_dir_all(&logs).unwrap();
        let sidecar = tree.write(
            "run.json",
            r#"{"category":"Mythic+","duration":120,"start":1784396963000}"#,
        );
        tree.write("run.mp4", "media");
        let log = logs.join("WoWCombatLog.txt");
        fs::write(
            &log,
            concat!(
                "7/18/2026 19:49:23.0000  CHALLENGE_MODE_START,\"Dungeon\",2526,402,22,[]\n",
                "7/18/2026 19:50:00.0000  SPELL_CAST_SUCCESS,Player-1-A,\"Evoker-Realm\",0x512,0x0,0000000000000000,nil,0x80000000,0x80000000,390386,\"Fury of the Aspects\",0x40\n",
            ),
        )
        .unwrap();
        let storage = tree.storage();
        let context = ParseTimeContext::new(2026, 120);
        assert_eq!(
            storage
                .enrich_legacy_bloodlust(std::slice::from_ref(&logs), context)
                .enriched,
            0
        );
        let value: Value = serde_json::from_str(&fs::read_to_string(&sidecar).unwrap()).unwrap();
        assert!(value.get("bloodlustTimeline").is_none());

        let mut file = OpenOptions::new().append(true).open(&log).unwrap();
        writeln!(
            file,
            "7/18/2026 19:51:24.0000  CHALLENGE_MODE_END,2526,1,3,120000,0,0"
        )
        .unwrap();
        assert_eq!(
            storage
                .enrich_legacy_bloodlust(std::slice::from_ref(&logs), context)
                .enriched,
            1
        );
    }

    #[test]
    fn historical_dst_offset_can_differ_from_the_current_offset() {
        assert_eq!(
            rounded_timezone_offset(60 * 60 * 1_000, 120),
            Some(60 * 60 * 1_000)
        );
        assert_eq!(rounded_timezone_offset(5 * 60 * 60 * 1_000, 120), None);
    }

    #[test]
    fn classic_sidecars_are_not_enrichment_targets() {
        let tree = TempTree::new("bloodlust-classic-skip");
        let sidecar = tree.write(
            "classic.json",
            r#"{"category":"Mythic+","flavour":"Classic","duration":120,"start":1}"#,
        );
        let original = fs::read_to_string(&sidecar).unwrap();
        let mut failures = Vec::new();
        assert!(
            tree.storage()
                .legacy_enrichment_targets(&mut failures)
                .is_empty()
        );
        assert!(failures.is_empty());
        assert_eq!(fs::read_to_string(sidecar).unwrap(), original);
    }

    struct TempTree {
        root: PathBuf,
    }

    impl TempTree {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("wr-storage-{name}-{}", uuid::Uuid::new_v4()));
            let tree = Self { root };
            tree.storage().prepare().expect("prepare");
            fs::create_dir_all(tree.capture_root().join("replay")).expect("replay dir");
            fs::create_dir_all(tree.capture_root().join("regular")).expect("regular dir");
            tree
        }

        fn library(&self) -> PathBuf {
            self.root.join("recordings with space")
        }

        fn capture_root(&self) -> PathBuf {
            self.root.join("capture")
        }

        fn storage(&self) -> Storage {
            Storage::new(self.library(), self.capture_root())
        }

        fn write(&self, relative: &str, contents: &str) -> PathBuf {
            let path = self.library().join(relative);
            fs::create_dir_all(path.parent().expect("parent")).expect("dir");
            fs::write(&path, contents).expect("write");
            path
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// Copy every legacy fixture plus a placeholder media file for each.
    fn install_legacy_fixtures(tree: &TempTree) -> Vec<String> {
        let mut names = Vec::new();
        let mut entries: Vec<PathBuf> = fs::read_dir(fixture_dir())
            .expect("fixtures")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect();
        entries.sort();
        for path in entries {
            let name = path
                .file_stem()
                .expect("stem")
                .to_string_lossy()
                .into_owned();
            let json = fs::read_to_string(&path).expect("read fixture");
            tree.write(&format!("{name}.json"), &json);
            tree.write(&format!("{name}.mp4"), "fake media bytes");
            names.push(name);
        }
        names
    }

    #[derive(Serialize)]
    struct Snapshot {
        entries: Vec<Value>,
        correlations: Vec<Value>,
        skipped: Vec<String>,
        ignored_files: usize,
    }

    fn snapshot(index: &LibraryIndex, root: &Path) -> String {
        let entries = index
            .entries
            .iter()
            .map(|entry| {
                let mut value = serde_json::to_value(NativeSidecar::from_entry(entry, root))
                    .expect("serialize entry");
                value["legacy_sidecar"] = Value::Bool(index.legacy_ids.contains(&entry.id));
                value
            })
            .collect();
        let correlations = index
            .correlations
            .iter()
            .map(|correlated| {
                serde_json::json!({
                    "primary": correlated.primary_id.as_str(),
                    "local_pov_ids": correlated
                        .local_pov_ids
                        .iter()
                        .map(RecordingId::as_str)
                        .collect::<Vec<_>>(),
                })
            })
            .collect();
        let snapshot = Snapshot {
            entries,
            correlations,
            skipped: index
                .skipped
                .iter()
                .map(|skipped| {
                    format!(
                        "{}: {}",
                        skipped
                            .sidecar_path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy(),
                        skipped.reason
                    )
                })
                .collect(),
            ignored_files: index.ignored_files,
        };
        serde_json::to_string_pretty(&snapshot).expect("serialize snapshot")
    }

    #[test]
    fn legacy_sidecars_map_to_the_golden_and_stay_unmodified_on_disk() {
        let tree = TempTree::new("legacy-scan");
        let names = install_legacy_fixtures(&tree);
        // One unrelated file that must only be counted.
        tree.write("notes.txt", "not a recording");

        let storage = tree.storage();
        let index = storage.scan();
        assert_eq!(index.entries.len(), names.len());
        assert_eq!(index.ignored_files, 1);
        assert!(index.skipped.is_empty(), "{:?}", index.skipped);

        let actual = snapshot(&index, &tree.library());
        let golden = golden_path("legacy-scan.json");
        let expected = fs::read_to_string(&golden).unwrap_or_else(|error| {
            panic!("read {}: {error}\nactual:\n{actual}", golden.display())
        });
        assert_eq!(actual.trim(), expected.trim());

        // Reading the library never rewrites a legacy sidecar.
        for name in names {
            let original =
                fs::read_to_string(fixture_dir().join(format!("{name}.json"))).expect("fixture");
            let on_disk =
                fs::read_to_string(tree.library().join(format!("{name}.json"))).expect("scanned");
            assert_eq!(original, on_disk, "{name} was modified");
        }
    }

    fn draft(id: &RecordingId) -> RecordingDraft {
        RecordingDraft {
            id: id.clone(),
            category: Category::Raids,
            flavor: GameFlavor::Retail,
            started_at_ms: 1_772_323_200_000,
            overrun_ms: 15_000,
            details: ActivityDetails::Raid {
                zone_id: Some(2769),
                zone_name: Some("Undermine".to_owned()),
                encounter_id: Some(3009),
                encounter_name: Some("Chrome King Gallywix".to_owned()),
                difficulty_id: Some(16),
                difficulty: Some("M".to_owned()),
                pull: None,
                boss_percent: Some(0),
            },
            player: Some(PlayerSummary {
                name: "Testone".to_owned(),
                realm: Some("Testrealm".to_owned()),
                guid: Some("Player-1000-AAAA0001".to_owned()),
                class_id: None,
                spec_id: Some(577),
            }),
            combatants: Vec::new(),
            timeline: vec![
                TimelineItem::point(
                    TimelineKind::Death,
                    1_000,
                    Some("Testtwo".to_owned()),
                    Some(Outcome::Loss),
                    None,
                ),
                TimelineItem::span(TimelineKind::Encounter, 0, 60_000, None, None, None)
                    .expect("span"),
            ],
            outcome: Some(Outcome::Win),
            ended_at_ms: Some(1_772_323_260_000),
            duration_ms: Some(75_000),
            title: Some("Testone - Undermine, Chrome King Gallywix [M] (Kill)".to_owned()),
            activity_hash: Some("0f1e2d3c4b5a69788796a5b4c3d2e1f0".to_owned()),
        }
    }

    fn artifacts(tree: &TempTree, with_replay: bool) -> CaptureArtifacts {
        let regular = tree
            .capture_root()
            .join("regular/Video_2026-03-01_00-00-05.mkv");
        fs::write(&regular, "regular bytes").expect("regular");
        let replay = with_replay.then(|| {
            let replay = tree
                .capture_root()
                .join("replay/Replay_2026-03-01_00-00-00.mkv");
            fs::write(&replay, "replay bytes").expect("replay");
            replay
        });
        CaptureArtifacts {
            replay,
            regular,
            requested_replay_ms: 8_000,
            // The regular recording starts five seconds after the activity.
            regular_started_at_ms: 1_772_323_205_000,
            regular_stopped_at_ms: 1_772_323_275_000,
        }
    }

    #[test]
    fn scan_accepts_zero_byte_performance_placeholders_without_decoding() {
        let tree = TempTree::new("zero-byte-corpus");
        let storage = tree.storage();
        let names = install_legacy_fixtures(&tree);
        let media = tree.library().join(&names[0]).with_extension("mp4");
        fs::write(&media, []).expect("truncate placeholder media");

        let index = storage.scan();
        assert_eq!(index.entries.len(), names.len());
        assert!(index.skipped.is_empty());
        assert!(
            index
                .entries
                .iter()
                .find(|entry| entry.media_path == media)
                .is_some_and(|entry| !entry.media.has_content)
        );
    }

    #[test]
    fn finalize_writes_media_relative_markers_and_survives_a_rescan() {
        let tree = TempTree::new("finalize");
        let storage = tree.storage();
        let temp_media = tree.capture_root().join("staging/combined.mp4");
        fs::write(&temp_media, "final media bytes").expect("temp media");

        let id = RecordingId::new();
        let draft = draft(&id);
        let artifacts = artifacts(&tree, true);
        let entry = storage
            .finalize(
                &draft,
                &artifacts,
                &CombinedMedia {
                    temp_media: temp_media.clone(),
                    // Eight seconds of usable replay in front of a regular
                    // recording that started five seconds after the activity.
                    actual_replay_ms: 8_000,
                    facts: MediaFacts {
                        fps: Some(60),
                        width: None,
                        height: None,
                        codec: Some(Codec::H264),
                        has_content: true,
                    },
                },
            )
            .expect("finalize");

        // Media start is five seconds before the activity, so every marker
        // shifts by the usable replay lead-in.
        assert_eq!(entry.duration_ms, 8_000 + 70_000);
        assert_eq!(entry.timeline[0].start_ms(), 4_000);
        assert_eq!(entry.timeline[0].end_ms(), None);
        assert_eq!(entry.timeline[1].start_ms(), 3_000);
        assert_eq!(entry.timeline[1].end_ms(), Some(63_000));
        assert!(entry.media_path.starts_with(tree.library()));
        assert!(
            entry
                .media_path
                .to_string_lossy()
                .contains("Chrome King Gallywix [M] (Kill)")
        );
        assert_eq!(
            fs::read_to_string(&entry.media_path).expect("media"),
            "final media bytes"
        );
        assert!(!temp_media.exists());
        // Intermediates are gone only after both final names exist.
        assert!(!artifacts.regular.exists());
        assert!(!artifacts.replay.expect("replay").exists());

        let index = storage.scan();
        assert_eq!(index.entries.as_ref(), &vec![entry.clone()]);
        assert!(index.legacy_ids.is_empty());
        assert_eq!(index.correlations.len(), 1);
    }

    #[test]
    fn regular_only_finalization_clips_markers_before_the_media_start() {
        let tree = TempTree::new("finalize-regular");
        let storage = tree.storage();
        let temp_media = tree.capture_root().join("staging/combined.mp4");
        fs::write(&temp_media, "final media bytes").expect("temp media");

        let entry = storage
            .finalize(
                &draft(&RecordingId::new()),
                &artifacts(&tree, false),
                &CombinedMedia {
                    temp_media,
                    actual_replay_ms: 0,
                    facts: MediaFacts {
                        fps: None,
                        width: None,
                        height: None,
                        codec: None,
                        has_content: true,
                    },
                },
            )
            .expect("finalize");

        assert_eq!(entry.duration_ms, 70_000);
        // The death at +1 s and the first five seconds of the encounter span
        // are not in the media; the point is dropped, the span is truncated.
        assert_eq!(entry.timeline.len(), 1);
        assert_eq!(entry.timeline[0].start_ms(), 0);
        assert_eq!(entry.timeline[0].end_ms(), Some(55_000));
    }

    #[test]
    fn startup_sweep_quarantines_interruption_leftovers_only() {
        let tree = TempTree::new("sweep");
        let storage = tree.storage();
        install_legacy_fixtures(&tree);
        let kept = tree.library().join("manual.mp4");

        // One row per interruption state the architecture can actually create.
        let orphan_media = tree.write("activity-1772323200000-orphan.mp4", "media with no sidecar");
        let orphan_temp = tree.write("activity-1772323200000-orphan.json.tmp", "{}");
        let replay = tree
            .capture_root()
            .join("replay/Replay_2026-03-01_00-00-00.mkv");
        fs::write(&replay, "replay").expect("replay");
        let regular = tree
            .capture_root()
            .join("regular/Video_2026-03-01_00-00-05.mkv");
        fs::write(&regular, "regular").expect("regular");
        let staging = tree.capture_root().join("staging/replay-trim-1.mkv");
        fs::write(&staging, "trim").expect("staging");

        let report = storage.sweep_orphans();
        assert!(report.failures.is_empty(), "{:?}", report.failures);
        assert_eq!(report.quarantined.len(), 5);
        for original in [&orphan_media, &orphan_temp, &replay, &regular, &staging] {
            assert!(!original.exists(), "{} was not swept", original.display());
        }
        for moved in &report.quarantined {
            assert!(moved.starts_with(tree.library().join(RECOVERY_DIR)));
            let reason = moved.with_file_name(format!(
                "{}.recovery.txt",
                moved.file_name().expect("name").to_string_lossy()
            ));
            assert!(
                fs::read_to_string(reason)
                    .expect("reason")
                    .contains("original path:")
            );
        }

        // Sidecar-referenced media is untouched, and the library still scans.
        assert!(kept.exists());
        assert_eq!(storage.scan().entries.len(), 11);
    }

    #[test]
    fn updates_rewrite_native_sidecars_and_patch_legacy_ones_in_place() {
        let tree = TempTree::new("update");
        let storage = tree.storage();
        install_legacy_fixtures(&tree);
        let index = storage.scan();

        let legacy = index
            .entries
            .iter()
            .find(|entry| entry.id.as_str().starts_with("arena-2v2"))
            .expect("legacy entry")
            .clone();
        assert!(index.legacy_ids.contains(&legacy.id));

        let tagged = storage
            .update(&legacy, &EntryUpdate::Tag(" nice one ".to_owned()))
            .expect("tag");
        let protected = storage
            .update(&tagged, &EntryUpdate::Protected(true))
            .expect("protect");
        assert_eq!(protected.tag.as_deref(), Some(" nice one "));
        assert!(protected.protected);

        let patched: Value =
            serde_json::from_str(&fs::read_to_string(&legacy.sidecar_path).expect("read"))
                .expect("json");
        assert_eq!(patched["tag"], Value::String(" nice one ".to_owned()));
        assert_eq!(patched["protected"], Value::Bool(true));
        // Unknown and legacy-only fields survive untouched for the final AppImage.
        assert_eq!(patched["teamMMR"], Value::from(1850));
        assert_eq!(
            patched["uniqueHash"],
            Value::from("aa00bb11cc22dd33ee44ff5566778899")
        );
        assert!(patched.get("schema_version").is_none());
        assert!(patched["combatants"].as_array().expect("combatants").len() == 2);

        // Clearing removes the key exactly like the baseline's undefined tag.
        storage
            .update(&protected, &EntryUpdate::Tag("   ".to_owned()))
            .expect("clear tag");
        let cleared: Value =
            serde_json::from_str(&fs::read_to_string(&legacy.sidecar_path).expect("read"))
                .expect("json");
        assert!(cleared.get("tag").is_none());

        // A native sidecar round-trips through the typed model.
        let temp_media = tree.capture_root().join("staging/native.mp4");
        fs::write(&temp_media, "media").expect("media");
        let native = storage
            .finalize(
                &draft(&RecordingId::new()),
                &artifacts(&tree, false),
                &CombinedMedia {
                    temp_media,
                    actual_replay_ms: 0,
                    facts: MediaFacts {
                        fps: Some(60),
                        width: None,
                        height: None,
                        codec: Some(Codec::H264),
                        has_content: true,
                    },
                },
            )
            .expect("finalize");
        let updated = storage
            .update(&native, &EntryUpdate::Protected(true))
            .expect("protect native");
        let reloaded = storage
            .scan()
            .entries
            .iter()
            .find(|entry| entry.id == native.id)
            .cloned()
            .expect("rescan");
        assert_eq!(reloaded, updated);
    }

    #[test]
    fn deletion_reports_per_entry_failures_and_refuses_paths_outside_the_root() {
        let tree = TempTree::new("delete");
        let storage = tree.storage();
        install_legacy_fixtures(&tree);
        let index = storage.scan();

        let mut good = index.entries[0].clone();
        let mut missing = index.entries[1].clone();
        fs::remove_file(&missing.media_path).expect("remove media");
        let mut outside = index.entries[2].clone();
        outside.media_path = tree.root.join("escaped.mp4");
        fs::write(&outside.media_path, "outside").expect("outside");

        let result = storage.delete(&[good.clone(), missing.clone(), outside.clone()]);
        assert_eq!(result.deleted, vec![good.id.clone()]);
        assert_eq!(result.failures.len(), 2);
        assert!(result.failures[0].1.contains("media"));
        assert!(result.failures[1].1.contains("not a direct child"));
        assert!(!good.media_path.exists() && !good.sidecar_path.exists());
        assert!(outside.media_path.exists());

        // The sidecar of a partially removed entry stays for the next scan.
        good.media_path = PathBuf::new();
        missing.media_path = PathBuf::new();
        assert!(missing.sidecar_path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn deletion_refuses_a_path_through_a_symlinked_directory() {
        use std::os::unix::fs::symlink;

        let tree = TempTree::new("delete-parent-symlink");
        let storage = tree.storage();
        install_legacy_fixtures(&tree);
        let mut entry = storage.scan().entries[0].clone();

        let outside = tree.root.join("outside");
        fs::create_dir(&outside).expect("outside directory");
        let media = outside.join("escaped.mp4");
        let sidecar = outside.join("escaped.json");
        fs::write(&media, "outside media").expect("outside media");
        fs::write(&sidecar, "{}").expect("outside sidecar");
        let link = tree.library().join("linked");
        symlink(&outside, &link).expect("directory symlink");
        entry.media_path = link.join("escaped.mp4");
        entry.sidecar_path = link.join("escaped.json");

        let result = storage.delete(&[entry]);
        assert!(result.deleted.is_empty());
        assert_eq!(result.failures.len(), 1);
        assert!(result.failures[0].1.contains("not a direct child"));
        assert!(media.exists() && sidecar.exists());
    }

    #[test]
    fn storage_limits_evict_only_unprotected_recordings_oldest_first() {
        let tree = TempTree::new("evict");
        let storage = tree.storage();
        install_legacy_fixtures(&tree);
        let entries = storage.scan().entries;

        // Unlimited never evicts.
        let unlimited = storage.enforce_limit(StorageLimit::Unlimited, &entries);
        assert!(unlimited.evicted.is_empty());
        assert!(unlimited.remaining_bytes > 0);
        assert_eq!(storage.scan().entries.len(), entries.len());

        // A limit far above usage evicts nothing either.
        let gib = StorageLimit::Gib(NonZeroU64::new(1).expect("nonzero"));
        assert!(storage.enforce_limit(gib, &entries).evicted.is_empty());

        // Give every media file a real size, then force eviction with a
        // one-GiB limit by pretending the library is larger: use padded files.
        let big = 400 * 1024;
        for entry in entries.iter() {
            fs::write(&entry.media_path, vec![0u8; big]).expect("pad");
        }
        let entries = storage.scan().entries;
        let protected: Vec<&LibraryEntry> =
            entries.iter().filter(|entry| entry.protected).collect();
        assert_eq!(protected.len(), 2);

        let tiny = Storage::new(tree.library(), tree.capture_root());
        let result = tiny.enforce_limit(
            StorageLimit::Gib(NonZeroU64::new(1).expect("nonzero")),
            &entries,
        );
        assert!(
            result.evicted.is_empty(),
            "1 GiB fits the whole fixture library"
        );

        // Shrink the limit below the protected content to prove the report.
        let mut protected_entries: Vec<LibraryEntry> = entries
            .iter()
            .filter(|entry| entry.protected)
            .cloned()
            .collect();
        protected_entries.push(
            entries
                .iter()
                .find(|entry| !entry.protected)
                .expect("unprotected")
                .clone(),
        );
        for entry in &protected_entries {
            fs::write(&entry.media_path, vec![0u8; 512 * 1024 * 1024]).expect("pad");
        }
        let result = storage.enforce_limit(
            StorageLimit::Gib(NonZeroU64::new(1).expect("nonzero")),
            &protected_entries,
        );
        assert!(result.protected_over_limit);
        assert_eq!(result.evicted.len(), 1);
        let evicted = protected_entries
            .iter()
            .find(|entry| result.evicted.contains(&entry.id))
            .expect("evicted entry");
        assert!(!evicted.protected);
        assert!(!evicted.media_path.exists());
        for entry in protected_entries.iter().filter(|entry| entry.protected) {
            assert!(entry.media_path.exists(), "protected entry was evicted");
        }
    }

    #[test]
    fn invalid_sidecars_are_skipped_with_a_diagnostic() {
        let tree = TempTree::new("skip");
        let storage = tree.storage();
        tree.write("broken.json", "{ not json");
        tree.write("broken.mp4", "media");
        tree.write(
            "no-media.json",
            r#"{"category":"Raids","duration":10,"start":1}"#,
        );
        tree.write("no-category.json", r#"{"duration":10,"start":1}"#);
        tree.write("no-category.mp4", "media");

        let index = storage.scan();
        assert!(index.entries.is_empty());
        assert_eq!(index.skipped.len(), 3);
        assert!(
            index
                .skipped
                .iter()
                .any(|skipped| skipped.reason.contains("invalid JSON"))
        );
        assert!(
            index
                .skipped
                .iter()
                .any(|skipped| skipped.reason.contains("media file"))
        );
        assert!(
            index
                .skipped
                .iter()
                .any(|skipped| skipped.reason.contains("missing category"))
        );
    }
}
