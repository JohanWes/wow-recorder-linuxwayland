// SPDX-License-Identifier: GPL-3.0-or-later

//! Serialized media and storage jobs.
//!
//! One worker thread consumes `MediaJob`s in order: perform one-time legacy
//! timeline enrichment, finalize a finished capture, or cut a clip. FFmpeg is spawned
//! directly with `std::process::Command` — no shell, no wrapper crate, no
//! reader thread. Every invocation writes progress to an exclusively created
//! `-progress` file and stderr to an exclusively created log file, which the
//! single poll loop reads incrementally.
//!
//! Shutdown arrives on a separate capacity-one control channel and is observed
//! within one polling interval: an automatic finalization gets a bounded grace
//! period, user clip jobs are cancelled immediately, and both
//! escalate SIGINT then `Child::kill`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::activity::RecordingDraft;
use crate::domain::{
    ActivityDetails, Category, LibraryEntry, MediaFacts, RecordingId, TimelineItem, TimelineShape,
    WorkKind, WorkProgress,
};
use crate::parser::ParseTimeContext;
use crate::process;
use crate::recorder::CaptureArtifacts;
use crate::storage::{CombinedMedia, Storage, now_unix_ms, sanitize_name, unique_stem};

/// Bytes of the per-job stderr log read back for diagnostics.
const LOG_TAIL_BYTES: u64 = 8 * 1024;
/// Presenting transcode progress more often is visually indistinguishable but
/// makes every GTK snapshot repeat work unrelated to the progress label.
const PROGRESS_EMIT_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Debug)]
pub struct MediaConfig {
    /// FFmpeg executable; the production value is the one bundled by WR-002.
    pub ffmpeg: PathBuf,
    /// Local UTC offset used for generated display names, supplied by the
    /// coordinator exactly like the parser's time context (WR-004).
    pub utc_offset_minutes: i32,
    /// Poll interval for control and FFmpeg progress.
    pub poll_interval: Duration,
    /// How long an in-flight automatic finalization may continue after a
    /// shutdown request before it is interrupted.
    pub finalize_grace: Duration,
    /// How long a SIGINT is given before `Child::kill`.
    pub sigint_grace: Duration,
}

impl Default for MediaConfig {
    fn default() -> Self {
        Self {
            ffmpeg: PathBuf::from("ffmpeg"),
            utc_offset_minutes: 0,
            poll_interval: Duration::from_millis(50),
            finalize_grace: Duration::from_secs(30),
            sigint_grace: Duration::from_secs(2),
        }
    }
}

pub enum MediaJob {
    EnrichLegacyBloodlust {
        retail_log_dirs: Vec<PathBuf>,
        context: ParseTimeContext,
    },
    FinalizeRecording {
        draft: Box<RecordingDraft>,
        artifacts: CaptureArtifacts,
        facts: MediaFacts,
    },
    CreateClip {
        source: Box<LibraryEntry>,
        start_ms: u64,
        end_ms: u64,
    },
}

impl MediaJob {
    pub fn kind(&self) -> WorkKind {
        match self {
            Self::EnrichLegacyBloodlust { .. } => {
                unreachable!("maintenance jobs do not enter the media work queues")
            }
            Self::FinalizeRecording { .. } => WorkKind::Finalize,
            Self::CreateClip { .. } => WorkKind::Clip,
        }
    }
}

pub enum MediaControl {
    /// Stop optional startup maintenance so recording finalization can use the
    /// sole worker. Ignored once a real media job is running.
    CancelMaintenance,
    Shutdown {
        pending_finalizations: Vec<MediaJob>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MediaEvent {
    TimelineEnriched {
        enriched: usize,
        failures: Vec<String>,
    },
    Progress(WorkProgress),
    Completed {
        kind: WorkKind,
        entry: Box<LibraryEntry>,
    },
    /// The recording produced no library entry (discarded draft, failed job).
    Failed {
        kind: WorkKind,
        message: String,
    },
    Cancelled {
        kind: WorkKind,
    },
}

pub struct MediaWorker {
    config: MediaConfig,
    storage: Storage,
    jobs: Receiver<MediaJob>,
    control: Receiver<MediaControl>,
    events: Sender<MediaEvent>,
    shutdown_at: Option<Instant>,
    shutdown_finalizations: std::collections::VecDeque<MediaJob>,
}

impl MediaWorker {
    pub fn new(
        config: MediaConfig,
        storage: Storage,
        jobs: Receiver<MediaJob>,
        control: Receiver<MediaControl>,
        events: Sender<MediaEvent>,
    ) -> Self {
        Self {
            config,
            storage,
            jobs,
            control,
            events,
            shutdown_at: None,
            shutdown_finalizations: std::collections::VecDeque::new(),
        }
    }

    /// Consume jobs until shutdown is requested or the job channel closes.
    pub fn run(mut self) {
        loop {
            if self.shutdown_at.is_some() {
                while let Some(job) = self.shutdown_finalizations.pop_front() {
                    self.run_job(job);
                }
                break;
            }
            match self.jobs.recv_timeout(self.config.poll_interval) {
                Ok(job) => self.run_job(job),
                Err(RecvTimeoutError::Timeout) => self.observe_shutdown(),
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    fn observe_shutdown(&mut self) {
        if self.shutdown_at.is_some() {
            return;
        }
        match self.control.try_recv() {
            Ok(MediaControl::Shutdown {
                pending_finalizations,
            }) => self.begin_shutdown(pending_finalizations),
            Ok(MediaControl::CancelMaintenance) => {}
            Err(TryRecvError::Disconnected) => {
                self.begin_shutdown(Vec::new());
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    fn begin_shutdown(&mut self, pending_finalizations: Vec<MediaJob>) {
        if self.shutdown_at.is_none() {
            self.shutdown_at = Some(Instant::now());
            self.shutdown_finalizations.extend(pending_finalizations);
        }
    }

    fn emit(&self, event: MediaEvent) {
        let _ = self.events.send(event);
    }

    fn run_job(&mut self, job: MediaJob) {
        let job = match job {
            MediaJob::EnrichLegacyBloodlust {
                retail_log_dirs,
                context,
            } => {
                let control = &self.control;
                let mut shutdown_finalizations = None;
                let mut cancellation_observed = false;
                let report = self.storage.enrich_legacy_bloodlust_cancellable(
                    &retail_log_dirs,
                    context,
                    || {
                        if cancellation_observed {
                            return true;
                        }
                        cancellation_observed = match control.try_recv() {
                            Ok(MediaControl::CancelMaintenance) => true,
                            Ok(MediaControl::Shutdown {
                                pending_finalizations,
                            }) => {
                                shutdown_finalizations = Some(pending_finalizations);
                                true
                            }
                            Err(TryRecvError::Disconnected) => {
                                shutdown_finalizations = Some(Vec::new());
                                true
                            }
                            Err(TryRecvError::Empty) => false,
                        };
                        cancellation_observed
                    },
                );
                if let Some(pending) = shutdown_finalizations {
                    self.begin_shutdown(pending);
                }
                self.emit(MediaEvent::TimelineEnriched {
                    enriched: report.enriched,
                    failures: report.failures,
                });
                return;
            }
            job => job,
        };
        let kind = job.kind();
        let outcome = match job {
            MediaJob::EnrichLegacyBloodlust { .. } => {
                unreachable!("maintenance job returned from the early match")
            }
            MediaJob::FinalizeRecording {
                draft,
                artifacts,
                facts,
            } => self.finalize_recording(&draft, &artifacts, facts),
            MediaJob::CreateClip {
                source,
                start_ms,
                end_ms,
            } => self.create_clip(&source, start_ms, end_ms),
        };
        match outcome {
            Ok(Some(entry)) => self.emit(MediaEvent::Completed {
                kind,
                entry: Box::new(entry),
            }),
            Ok(None) => self.emit(MediaEvent::Cancelled { kind }),
            Err(message) => self.emit(MediaEvent::Failed { kind, message }),
        }
    }

    // -----------------------------------------------------------------------
    // Finalization
    // -----------------------------------------------------------------------

    fn finalize_recording(
        &mut self,
        draft: &RecordingDraft,
        artifacts: &CaptureArtifacts,
        facts: MediaFacts,
    ) -> Result<Option<LibraryEntry>, String> {
        let final_temp = self
            .job_file("final", "mp4")
            .map_err(|error| format!("final media temp: {error}"))?;

        let combined = match self.combine(artifacts, &final_temp)? {
            Some(actual_replay_ms) => CombinedMedia {
                temp_media: final_temp.clone(),
                actual_replay_ms,
                facts,
            },
            None => {
                let _ = fs::remove_file(&final_temp);
                return Ok(None);
            }
        };

        if fs::metadata(&combined.temp_media)
            .map(|meta| meta.len())
            .unwrap_or(0)
            == 0
        {
            let _ = fs::remove_file(&combined.temp_media);
            return Err("FFmpeg produced an empty recording".to_owned());
        }

        self.storage
            .finalize(draft, artifacts, &combined)
            .map(Some)
            .map_err(|error| {
                let _ = fs::remove_file(&combined.temp_media);
                format!("finalize: {error}")
            })
    }

    /// Trim the replay to the requested lead-in and concatenate it in front of
    /// the regular recording. Returns the usable replay milliseconds, or `None`
    /// when the job was cancelled. Any replay problem falls back to the regular
    /// recording alone with zero replay, matching the baseline.
    fn combine(
        &mut self,
        artifacts: &CaptureArtifacts,
        final_temp: &Path,
    ) -> Result<Option<u64>, String> {
        let regular_only = |worker: &Self| -> Result<Option<u64>, String> {
            let _ = worker;
            fs::copy(&artifacts.regular, final_temp)
                .map(|_| Some(0))
                .map_err(|error| format!("copy regular recording: {error}"))
        };

        let Some(replay) = artifacts.replay.as_deref().filter(|path| path.exists()) else {
            return regular_only(self);
        };

        let trim_temp = self
            .job_file("replay-trim", "mkv")
            .map_err(|error| format!("replay trim temp: {error}"))?;
        let wanted_seconds = (artifacts.requested_replay_ms as f64 / 1000.0)
            .round()
            .max(1.0) as u64;
        let trim = self.run_ffmpeg(
            WorkKind::Finalize,
            trim_args(replay, wanted_seconds, &trim_temp),
            None,
        );

        let trimmed_out_time_ms = match trim {
            FfmpegOutcome::Done { out_time_ms } if out_time_ms > 0 => out_time_ms,
            FfmpegOutcome::Cancelled => {
                let _ = fs::remove_file(&trim_temp);
                return Ok(None);
            }
            _ => {
                let _ = fs::remove_file(&trim_temp);
                return regular_only(self);
            }
        };

        // `-sseof` reports the seek-relative output time, which is shorter than
        // the keyframe-aligned file the stream copy actually wrote (measured at
        // ~0.9 s on the WR-006 captures). Remuxing the trim to the null sink is
        // one cheap pass that yields the real lead-in; without it every marker
        // would sit that far off. The trim's own progress is the fallback.
        let actual_replay_ms =
            match self.run_ffmpeg(WorkKind::Finalize, measure_args(&trim_temp), None) {
                FfmpegOutcome::Done { out_time_ms } if out_time_ms > 0 => out_time_ms,
                FfmpegOutcome::Cancelled => {
                    let _ = fs::remove_file(&trim_temp);
                    return Ok(None);
                }
                _ => trimmed_out_time_ms,
            };

        let list_temp = self
            .job_file("concat", "txt")
            .map_err(|error| format!("concat list temp: {error}"))?;
        let list = format!(
            "file '{}'\nfile '{}'\n",
            escape_concat(&trim_temp),
            escape_concat(&artifacts.regular)
        );
        if let Err(error) = fs::write(&list_temp, list) {
            let _ = fs::remove_file(&trim_temp);
            let _ = fs::remove_file(&list_temp);
            return regular_only(self).map_err(|_| format!("write concat list: {error}"));
        }

        let concat = self.run_ffmpeg(
            WorkKind::Finalize,
            concat_args(&list_temp, final_temp),
            None,
        );
        let _ = fs::remove_file(&trim_temp);
        let _ = fs::remove_file(&list_temp);

        match concat {
            FfmpegOutcome::Done { .. } => Ok(Some(actual_replay_ms)),
            FfmpegOutcome::Cancelled => Ok(None),
            FfmpegOutcome::Failed { .. } => regular_only(self),
        }
    }

    // -----------------------------------------------------------------------
    // Clips
    // -----------------------------------------------------------------------

    fn create_clip(
        &mut self,
        source: &LibraryEntry,
        start_ms: u64,
        end_ms: u64,
    ) -> Result<Option<LibraryEntry>, String> {
        if end_ms <= start_ms || end_ms > source.duration_ms {
            return Err(format!(
                "clip range {start_ms}..{end_ms} ms is outside the {} ms source",
                source.duration_ms
            ));
        }
        if !source.media_path.exists() {
            return Err(format!(
                "clip source {} is missing",
                source.media_path.display()
            ));
        }

        let duration_ms = end_ms - start_ms;
        let created_at = now_unix_ms();
        let id = RecordingId::new();
        let stem = unique_stem(
            &id,
            created_at,
            &format!(
                "{} - Clipped at {}",
                sanitize_name(&source.title),
                format_local_stamp(created_at, self.config.utc_offset_minutes)
            ),
        );
        let (media_path, sidecar_path) = self
            .storage
            .claim_output(&stem)
            .map_err(|error| format!("clip output: {error}"))?;

        let temp = self
            .job_file("clip", "mp4")
            .map_err(|error| format!("clip temp: {error}"))?;
        let outcome = self.run_ffmpeg(
            WorkKind::Clip,
            clip_args(&source.media_path, start_ms, duration_ms, &temp),
            Some(duration_ms),
        );

        match outcome {
            FfmpegOutcome::Done { .. } => {}
            FfmpegOutcome::Cancelled => {
                let _ = fs::remove_file(&temp);
                let _ = fs::remove_file(&media_path);
                return Ok(None);
            }
            FfmpegOutcome::Failed { message } => {
                let _ = fs::remove_file(&temp);
                let _ = fs::remove_file(&media_path);
                return Err(message);
            }
        }

        let entry = LibraryEntry {
            id,
            media_path,
            sidecar_path,
            category: Category::Clip,
            flavor: source.flavor.clone(),
            title: source.title.clone(),
            start_unix_ms: created_at,
            duration_ms,
            outcome: source.outcome,
            // Clips are protected in the baseline so eviction cannot reclaim
            // work the user deliberately cut.
            protected: true,
            tag: None,
            activity_hash: source.activity_hash.clone(),
            player: source.player.clone(),
            combatants: source.combatants.clone(),
            details: ActivityDetails::Clip {
                source_recording: source.id.clone(),
                source_category: source.category.clone(),
                source_title: Some(source.title.clone()),
            },
            timeline: clip_timeline(&source.timeline, start_ms, end_ms),
            media: source.media.clone(),
        };

        self.storage
            .write_new_entry(&entry, &temp)
            .map_err(|error| {
                let _ = fs::remove_file(&temp);
                format!("write clip: {error}")
            })?;
        Ok(Some(entry))
    }

    // -----------------------------------------------------------------------
    // Kill videos
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // FFmpeg
    // -----------------------------------------------------------------------

    /// Exclusively create a per-job file in the capture staging directory.
    fn job_file(&self, prefix: &str, extension: &str) -> io::Result<PathBuf> {
        let staging = self.storage.staging_dir();
        fs::create_dir_all(staging)?;
        let path = staging.join(format!("{prefix}-{}.{extension}", Uuid::new_v4()));
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map(|_| path)
    }

    /// Spawn FFmpeg and poll it. Progress and stderr go to exclusively created
    /// per-job files (never pipes); control is observed within one interval.
    fn run_ffmpeg(
        &mut self,
        kind: WorkKind,
        args: Vec<String>,
        total_ms: Option<u64>,
    ) -> FfmpegOutcome {
        let progress_path = match self.job_file("progress", "txt") {
            Ok(path) => path,
            Err(error) => {
                return FfmpegOutcome::Failed {
                    message: format!("progress file: {error}"),
                };
            }
        };
        let log_path = match self.job_file("ffmpeg", "log") {
            Ok(path) => path,
            Err(error) => {
                let _ = fs::remove_file(&progress_path);
                return FfmpegOutcome::Failed {
                    message: format!("log file: {error}"),
                };
            }
        };

        let log = match File::create(&log_path) {
            Ok(file) => file,
            Err(error) => {
                let _ = fs::remove_file(&progress_path);
                let _ = fs::remove_file(&log_path);
                return FfmpegOutcome::Failed {
                    message: format!("log file: {error}"),
                };
            }
        };

        let mut command = Command::new(&self.config.ffmpeg);
        command
            // Stable Flatpak constrains the GTK process allocator arenas for
            // its RSS gate; media tools must retain their own defaults.
            .env_remove("MALLOC_ARENA_MAX")
            .arg("-progress")
            .arg(&progress_path)
            .arg("-nostats")
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log));

        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = fs::remove_file(&progress_path);
                let _ = fs::remove_file(&log_path);
                return FfmpegOutcome::Failed {
                    message: format!("spawn FFmpeg: {error}"),
                };
            }
        };

        let outcome = self.poll_child(kind, &mut child, &progress_path, total_ms);
        let status_message = match &outcome {
            FfmpegOutcome::Failed { message } => {
                let tail = read_log_tail(&log_path);
                Some(if tail.is_empty() {
                    message.clone()
                } else {
                    format!("{message}: {tail}")
                })
            }
            _ => None,
        };
        let _ = fs::remove_file(&progress_path);
        let _ = fs::remove_file(&log_path);

        match (outcome, status_message) {
            (FfmpegOutcome::Failed { .. }, Some(message)) => FfmpegOutcome::Failed { message },
            (other, _) => other,
        }
    }

    fn poll_child(
        &mut self,
        kind: WorkKind,
        child: &mut Child,
        progress_path: &Path,
        total_ms: Option<u64>,
    ) -> FfmpegOutcome {
        let mut progress = ProgressReader::new(progress_path);
        let mut out_time_ms = 0u64;
        let mut last_progress_emit = Instant::now() - PROGRESS_EMIT_INTERVAL;
        let mut interrupt_at = self.shutdown_at.map(|shutdown_at| match kind {
            WorkKind::Finalize => shutdown_at + self.config.finalize_grace,
            _ => Instant::now(),
        });

        loop {
            if interrupt_at.is_some() {
                // Shutdown is one-shot: once observed there is nothing more to
                // read, and polling a disconnected channel would busy-spin.
                std::thread::sleep(self.config.poll_interval);
            } else {
                match self.control.recv_timeout(self.config.poll_interval) {
                    Ok(MediaControl::Shutdown {
                        pending_finalizations,
                    }) => {
                        self.begin_shutdown(pending_finalizations);
                        // Automatic finalization may finish inside the grace
                        // period; user jobs are cancelled immediately. The
                        // deadline is set once: a disconnected control channel
                        // must not keep pushing it into the future.
                        interrupt_at = Some(match kind {
                            WorkKind::Finalize => Instant::now() + self.config.finalize_grace,
                            _ => Instant::now(),
                        });
                    }
                    Ok(MediaControl::CancelMaintenance) => {}
                    Err(RecvTimeoutError::Disconnected) => {
                        self.begin_shutdown(Vec::new());
                        interrupt_at = Some(match kind {
                            WorkKind::Finalize => Instant::now() + self.config.finalize_grace,
                            _ => Instant::now(),
                        });
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                }
            }

            if let Some(latest) = progress.read() {
                out_time_ms = latest;
                if last_progress_emit.elapsed() >= PROGRESS_EMIT_INTERVAL {
                    self.emit(MediaEvent::Progress(WorkProgress {
                        kind,
                        completed: latest,
                        total: total_ms,
                    }));
                    last_progress_emit = Instant::now();
                }
            }

            match child.try_wait() {
                Ok(Some(status)) => {
                    if let Some(latest) = progress.read() {
                        out_time_ms = latest;
                    }
                    return if status.success() {
                        FfmpegOutcome::Done { out_time_ms }
                    } else if interrupt_at.is_some() {
                        FfmpegOutcome::Cancelled
                    } else {
                        FfmpegOutcome::Failed {
                            message: format!("FFmpeg exited with {status}"),
                        }
                    };
                }
                Ok(None) => {}
                Err(error) => {
                    return FfmpegOutcome::Failed {
                        message: format!("wait for FFmpeg: {error}"),
                    };
                }
            }

            if interrupt_at.is_some_and(|deadline| Instant::now() >= deadline) {
                // SIGINT, then kill if it does not go away.
                let _ = process::terminate(child, self.config.sigint_grace);
                return FfmpegOutcome::Cancelled;
            }
        }
    }
}

enum FfmpegOutcome {
    Done { out_time_ms: u64 },
    Failed { message: String },
    Cancelled,
}

/// Incremental reader for FFmpeg's `-progress` file: keeps a byte offset and a
/// partial-line buffer so only complete `key=value` lines are consumed.
struct ProgressReader {
    path: PathBuf,
    offset: u64,
    partial: String,
}

impl ProgressReader {
    fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            offset: 0,
            partial: String::new(),
        }
    }

    /// Latest `out_time_us`/`out_time_ms` value, in milliseconds.
    fn read(&mut self) -> Option<u64> {
        let mut file = File::open(&self.path).ok()?;
        let size = file.metadata().ok()?.len();
        if size <= self.offset {
            return None;
        }
        file.seek(SeekFrom::Start(self.offset)).ok()?;
        let mut buffer = String::new();
        let read = file.read_to_string(&mut buffer).ok()?;
        self.offset += read as u64;
        self.partial.push_str(&buffer);

        let complete_to = self.partial.rfind('\n').map(|index| index + 1)?;
        let complete: String = self.partial.drain(..complete_to).collect();

        let mut latest = None;
        for line in complete.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "out_time_us" | "out_time_ms" => {
                    // FFmpeg reports microseconds in both keys; `out_time_ms`
                    // has been microseconds since 2017.
                    if let Ok(micros) = value.parse::<u64>() {
                        latest = Some(micros / 1000);
                    }
                }
                _ => {}
            }
        }
        latest
    }
}

fn read_log_tail(path: &Path) -> String {
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let Ok(size) = file.metadata().map(|meta| meta.len()) else {
        return String::new();
    };
    if file
        .seek(SeekFrom::Start(size.saturating_sub(LOG_TAIL_BYTES)))
        .is_err()
    {
        return String::new();
    }
    let mut tail = Vec::new();
    let _ = file.read_to_end(&mut tail);
    String::from_utf8_lossy(&tail).trim().to_owned()
}

// ---------------------------------------------------------------------------
// FFmpeg argument builders
// ---------------------------------------------------------------------------

/// Take the final `seconds` of the replay without needing its duration, exactly
/// as the baseline does.
fn trim_args(replay: &Path, seconds: u64, output: &Path) -> Vec<String> {
    vec![
        "-nostdin".to_owned(),
        "-hide_banner".to_owned(),
        "-sseof".to_owned(),
        format!("-{seconds}"),
        "-i".to_owned(),
        replay.to_string_lossy().into_owned(),
        "-c:v".to_owned(),
        "copy".to_owned(),
        "-c:a".to_owned(),
        "copy".to_owned(),
        "-avoid_negative_ts".to_owned(),
        "make_zero".to_owned(),
        "-y".to_owned(),
        output.to_string_lossy().into_owned(),
    ]
}

/// Remux to the null sink purely to learn a file's real duration; the bundled
/// FFmpeg has no `ffprobe` and the null muxer is not built in.
fn measure_args(media: &Path) -> Vec<String> {
    vec![
        "-nostdin".to_owned(),
        "-hide_banner".to_owned(),
        "-i".to_owned(),
        media.to_string_lossy().into_owned(),
        "-c".to_owned(),
        "copy".to_owned(),
        "-y".to_owned(),
        "-f".to_owned(),
        "matroska".to_owned(),
        "/dev/null".to_owned(),
    ]
}

fn concat_args(list: &Path, output: &Path) -> Vec<String> {
    vec![
        "-nostdin".to_owned(),
        "-hide_banner".to_owned(),
        "-f".to_owned(),
        "concat".to_owned(),
        "-safe".to_owned(),
        "0".to_owned(),
        "-i".to_owned(),
        list.to_string_lossy().into_owned(),
        "-c:v".to_owned(),
        "copy".to_owned(),
        "-c:a".to_owned(),
        "copy".to_owned(),
        "-avoid_negative_ts".to_owned(),
        "make_zero".to_owned(),
        "-y".to_owned(),
        output.to_string_lossy().into_owned(),
    ]
}

/// Baseline clip rule: input-side seek (`setStartTime` is a fluent-ffmpeg
/// input option) plus output-side duration, with a pure stream copy.
fn clip_args(source: &Path, start_ms: u64, duration_ms: u64, output: &Path) -> Vec<String> {
    vec![
        "-nostdin".to_owned(),
        "-hide_banner".to_owned(),
        "-ss".to_owned(),
        format_seconds(start_ms),
        "-i".to_owned(),
        source.to_string_lossy().into_owned(),
        "-t".to_owned(),
        format_seconds(duration_ms),
        "-c:v".to_owned(),
        "copy".to_owned(),
        "-c:a".to_owned(),
        "copy".to_owned(),
        "-avoid_negative_ts".to_owned(),
        "make_zero".to_owned(),
        "-movflags".to_owned(),
        "+faststart".to_owned(),
        "-y".to_owned(),
        output.to_string_lossy().into_owned(),
    ]
}

/*
fn kill_video_args(
    segments: &[KillSegment],
    width: u32,
    height: u32,
    fps: u32,
    audio: KillAudio,
    output: &Path,
) -> Vec<String> {
    let mut args = vec!["-nostdin".to_owned(), "-hide_banner".to_owned()];
    for segment in segments {
        args.push("-i".to_owned());
        args.push(segment.source.media_path.to_string_lossy().into_owned());
    }
    args.push("-filter_complex".to_owned());
    args.push(kill_video_filter(segments, width, height, fps, audio));
    args.push("-map".to_owned());
    args.push("[v]".to_owned());
    args.push("-map".to_owned());
    args.push(match audio {
        KillAudio::Switched => "[a]".to_owned(),
        KillAudio::Source(index) => format!("{index}:a"),
    });
    args.extend(
        [
            "-shortest",
            "-c:v",
            "libx264",
            "-crf",
            KILL_VIDEO_CRF,
            "-c:a",
            "aac",
            "-preset",
            "fast",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
            "-xerror",
            "-y",
        ]
        .map(str::to_owned),
    );
    args.push(output.to_string_lossy().into_owned());
    args
}

/// Trim, normalize, and cross-fade each viewpoint, then concatenate. Identical
/// in shape to the baseline's filter graph.
fn kill_video_filter(
    segments: &[KillSegment],
    width: u32,
    height: u32,
    fps: u32,
    audio: KillAudio,
) -> String {
    let mut filter = String::new();
    let switched = audio == KillAudio::Switched;

    for (index, segment) in segments.iter().enumerate() {
        let start = format_seconds(segment.start_ms);
        let stop = format_seconds(segment.end_ms);
        let segment_seconds = (segment.end_ms - segment.start_ms) as f64 / 1000.0;
        let fade_out_start =
            format_number((segment_seconds - KILL_VIDEO_FADE_SECONDS as f64).max(0.0));
        let fade_in = format!("t=in:st=0:d={KILL_VIDEO_FADE_SECONDS}");
        let fade_out = format!("t=out:st={fade_out_start}:d={KILL_VIDEO_FADE_SECONDS}");

        filter.push_str(&format!(
            "[{index}:v]trim=start={start}:end={stop},setpts=PTS-STARTPTS,\
fps={fps},scale={width}:-2,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,\
fade={fade_in},fade={fade_out}[v{index}];"
        ));
        if switched {
            filter.push_str(&format!(
                "[{index}:a]atrim=start={start}:end={stop},asetpts=PTS-STARTPTS,\
afade={fade_in},afade={fade_out}[a{index}];"
            ));
        }
    }

    let count = segments.len();
    if switched {
        let inputs: String = (0..count).map(|i| format!("[v{i}][a{i}]")).collect();
        filter.push_str(&format!("{inputs}concat=n={count}:v=1:a=1[v][a]"));
    } else {
        let inputs: String = (0..count).map(|i| format!("[v{i}]")).collect();
        filter.push_str(&format!("{inputs}concat=n={count}:v=1:a=0[v]"));
    }
    filter
}

fn validate_kill_video(
    segments: &[KillSegment],
    width: u32,
    height: u32,
    fps: u32,
    audio: KillAudio,
) -> Result<(), String> {
    if segments.len() < 2 {
        return Err("a kill video needs at least two viewpoints".to_owned());
    }
    let distinct: std::collections::HashSet<&RecordingId> =
        segments.iter().map(|segment| &segment.source.id).collect();
    if distinct.len() < 2 {
        return Err("a kill video needs at least two distinct sources".to_owned());
    }
    if width == 0 || height == 0 || fps == 0 {
        return Err(format!("invalid output {width}x{height} at {fps} fps"));
    }
    if let KillAudio::Source(index) = audio
        && index >= segments.len()
    {
        return Err(format!("audio source {index} is not one of the viewpoints"));
    }
    for segment in segments {
        if segment.end_ms <= segment.start_ms || segment.end_ms > segment.source.duration_ms {
            return Err(format!(
                "segment {}..{} ms is outside {}",
                segment.start_ms,
                segment.end_ms,
                segment.source.media_path.display()
            ));
        }
        if !segment.source.media_path.exists() {
            return Err(format!(
                "viewpoint {} is missing",
                segment.source.media_path.display()
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Naming, timeline, formatting
// ---------------------------------------------------------------------------

/// Baseline kill-video display name.
fn kill_video_name(first: &LibraryEntry, created_at_ms: i64, utc_offset_minutes: i32) -> String {
    let mut name = format_local_stamp(first.start_unix_ms, utc_offset_minutes);
    name.push_str(" - Multiview");
    // The baseline appends the encounter only when it also has a difficulty.
    if let ActivityDetails::Raid {
        encounter_name: Some(encounter),
        difficulty: Some(difficulty),
        ..
    } = &first.details
    {
        name.push_str(&format!(" - {encounter} [{difficulty}]"));
    }
    name.push_str(&format!(
        " - Rendered at {}",
        format_local_stamp(created_at_ms, utc_offset_minutes)
    ));
    name
}

/// Move timeline items into the clipped range, dropping what falls outside and
/// truncating spans that straddle a boundary.
*/
fn clip_timeline(timeline: &[TimelineItem], start_ms: u64, end_ms: u64) -> Vec<TimelineItem> {
    let mut clipped = Vec::new();
    for item in timeline {
        let item_start = item.start_ms();
        let item_end = item.end_ms().unwrap_or(item_start);
        if item_end < start_ms || item_start > end_ms {
            continue;
        }
        match item.shape() {
            TimelineShape::Point => {
                if item_start < start_ms {
                    continue;
                }
                clipped.push(TimelineItem::point(
                    item.kind().clone(),
                    item_start - start_ms,
                    item.label().map(str::to_owned),
                    item.outcome(),
                    item.player_reference().map(str::to_owned),
                ));
            }
            TimelineShape::Span => {
                let span_start = item_start.max(start_ms) - start_ms;
                let span_end = item_end.min(end_ms) - start_ms;
                if let Ok(span) = TimelineItem::span(
                    item.kind().clone(),
                    span_start,
                    span_end,
                    item.label().map(str::to_owned),
                    item.outcome(),
                    item.player_reference().map(str::to_owned),
                ) {
                    clipped.push(span);
                }
            }
        }
    }
    clipped
}

fn format_seconds(milliseconds: u64) -> String {
    format_number(milliseconds as f64 / 1000.0)
}

/// Trim trailing zeros so whole seconds stay readable in argv and goldens.
fn format_number(value: f64) -> String {
    let text = format!("{value:.3}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    if text.is_empty() {
        "0".to_owned()
    } else {
        text.to_owned()
    }
}

fn escape_concat(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "'\\''")
}

/// `YYYY-MM-DD HH-MM-SS` in the supplied local offset, matching the baseline's
/// OBS-style timestamps.
fn format_local_stamp(epoch_ms: i64, utc_offset_minutes: i32) -> String {
    let local_seconds = epoch_ms.div_euclid(1000) + i64::from(utc_offset_minutes) * 60;
    let days = local_seconds.div_euclid(86_400);
    let seconds_of_day = local_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}-{:02}-{:02}",
        seconds_of_day / 3_600,
        (seconds_of_day % 3_600) / 60,
        seconds_of_day % 60
    )
}

/// Howard Hinnant's civil-from-days algorithm.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = if days >= 0 { days } else { days - 146_096 } / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::{Sender, sync_channel};
    use std::thread;

    use crate::domain::{Codec, GameFlavor, Outcome, TimelineKind};
    use crate::storage::SIDECAR_SCHEMA_VERSION;

    fn fake_ffmpeg() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/native/bin/fake-ffmpeg.sh")
    }

    struct Harness {
        root: PathBuf,
        jobs: Sender<MediaJob>,
        control: std::sync::mpsc::SyncSender<MediaControl>,
        events: Receiver<MediaEvent>,
        worker: Option<thread::JoinHandle<()>>,
    }

    impl Harness {
        fn new(name: &str) -> Self {
            Self::with_finalize_grace(name, Duration::from_millis(0))
        }

        fn with_finalize_grace(name: &str, finalize_grace: Duration) -> Self {
            let root = std::env::temp_dir().join(format!("wr-media-{name}-{}", Uuid::new_v4()));
            let storage = Storage::new(root.join("recordings with space"), root.join("capture"));
            storage.prepare().expect("prepare");
            fs::create_dir_all(root.join("capture/replay")).expect("replay dir");
            fs::create_dir_all(root.join("capture/regular")).expect("regular dir");

            let (jobs, jobs_rx) = std::sync::mpsc::channel();
            let (control, control_rx) = sync_channel(1);
            let (events_tx, events) = std::sync::mpsc::channel();
            let config = MediaConfig {
                ffmpeg: fake_ffmpeg(),
                utc_offset_minutes: 0,
                poll_interval: Duration::from_millis(20),
                finalize_grace,
                sigint_grace: Duration::from_millis(400),
            };
            let worker = thread::spawn(move || {
                MediaWorker::new(config, storage, jobs_rx, control_rx, events_tx).run();
            });
            Self {
                root,
                jobs,
                control,
                events,
                worker: Some(worker),
            }
        }

        fn library(&self) -> PathBuf {
            self.root.join("recordings with space")
        }

        fn staging(&self) -> PathBuf {
            self.root.join("capture/staging")
        }

        fn set_mode(&self, mode: &str) {
            fs::write(self.staging().join("fake-ffmpeg-mode"), mode).expect("mode");
        }

        fn argv(&self) -> Vec<String> {
            fs::read_to_string(self.staging().join("fake-ffmpeg-argv.txt"))
                .expect("argv")
                .lines()
                .map(str::to_owned)
                .collect()
        }

        /// Wait for the next non-progress event.
        fn outcome(&self) -> MediaEvent {
            loop {
                match self
                    .events
                    .recv_timeout(Duration::from_secs(20))
                    .expect("media event")
                {
                    MediaEvent::Progress(_) => continue,
                    other => return other,
                }
            }
        }

        fn progress(&self) -> Vec<WorkProgress> {
            self.events
                .try_iter()
                .filter_map(|event| match event {
                    MediaEvent::Progress(progress) => Some(progress),
                    _ => None,
                })
                .collect()
        }

        fn source(&self, name: &str, duration_ms: u64) -> LibraryEntry {
            let media_path = self.library().join(format!("{name}.mp4"));
            fs::write(&media_path, "source media").expect("source media");
            LibraryEntry {
                id: RecordingId::new(),
                media_path,
                sidecar_path: self.library().join(format!("{name}.json")),
                category: Category::Raids,
                flavor: GameFlavor::Retail,
                title: format!("{name} - Chrome King Gallywix [M] (Kill)"),
                start_unix_ms: 1_772_323_200_000,
                duration_ms,
                outcome: Outcome::Win,
                protected: false,
                tag: None,
                activity_hash: Some("0f1e2d3c4b5a69788796a5b4c3d2e1f0".to_owned()),
                player: None,
                combatants: Vec::new(),
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
                timeline: vec![
                    TimelineItem::point(
                        TimelineKind::Death,
                        5_000,
                        Some("Early".to_owned()),
                        Some(Outcome::Loss),
                        None,
                    ),
                    TimelineItem::point(
                        TimelineKind::Death,
                        30_000,
                        Some("Inside".to_owned()),
                        Some(Outcome::Loss),
                        None,
                    ),
                    TimelineItem::span(TimelineKind::Encounter, 10_000, 40_000, None, None, None)
                        .expect("span"),
                ],
                media: MediaFacts {
                    fps: Some(60),
                    width: Some(1920),
                    height: Some(1080),
                    codec: Some(Codec::H264),
                    has_content: true,
                },
            }
        }

        fn shutdown_and_join(&mut self) {
            let _ = self.control.send(MediaControl::Shutdown {
                pending_finalizations: Vec::new(),
            });
            if let Some(worker) = self.worker.take() {
                worker.join().expect("join worker");
            }
        }
    }

    impl Drop for Harness {
        fn drop(&mut self) {
            drop(std::mem::replace(
                &mut self.jobs,
                std::sync::mpsc::channel().0,
            ));
            if let Some(worker) = self.worker.take() {
                let _ = self.control.try_send(MediaControl::Shutdown {
                    pending_finalizations: Vec::new(),
                });
                let _ = worker.join();
            }
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn any_running(pattern: &str) -> bool {
        Command::new("pgrep")
            .arg("-f")
            .arg(pattern)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false)
    }

    /*
    #[test]
    fn clip_and_kill_video_arguments_match_their_goldens() {
        let source = Path::new("/library dir/My Raid Kill.mp4");
        let output = Path::new("/staging dir/clip out.mp4");
        assert_golden(
            "clip-args.txt",
            &clip_args(source, 12_500, 30_000, output).join("\n"),
        );

        let first = LibraryEntry {
            media_path: PathBuf::from("/library dir/POV one.mp4"),
            ..Harness::new("golden-a").source("unused", 60_000)
        };
        let second = LibraryEntry {
            media_path: PathBuf::from("/library dir/POV two.mp4"),
            ..Harness::new("golden-b").source("unused", 60_000)
        };
        let segments = vec![
            KillSegment {
                source: first,
                start_ms: 4_000,
                end_ms: 14_000,
            },
            KillSegment {
                source: second,
                start_ms: 2_000,
                end_ms: 9_500,
            },
        ];
        assert_golden(
            "kill-video-args.txt",
            &kill_video_args(&segments, 1920, 1080, 30, KillAudio::Switched, output).join("\n"),
        );
        assert_golden(
            "kill-video-single-audio-args.txt",
            &kill_video_args(&segments, 1280, 720, 60, KillAudio::Source(1), output).join("\n"),
        );
    }

    */
    #[test]
    fn clip_job_writes_a_clips_entry_for_the_selected_interval() {
        let mut harness = Harness::new("clip");
        let source = harness.source("Raid POV", 60_000);
        harness
            .jobs
            .send(MediaJob::CreateClip {
                source: Box::new(source.clone()),
                start_ms: 10_000,
                end_ms: 35_000,
            })
            .expect("send clip");

        let MediaEvent::Completed { kind, entry } = harness.outcome() else {
            panic!("clip did not complete");
        };
        assert_eq!(kind, WorkKind::Clip);
        assert_eq!(entry.category, Category::Clip);
        assert_eq!(entry.duration_ms, 25_000);
        assert!(entry.protected);
        assert_eq!(
            entry.details,
            ActivityDetails::Clip {
                source_recording: source.id.clone(),
                source_category: Category::Raids,
                source_title: Some(source.title.clone()),
            }
        );
        // The early death is outside the clip; the later one and the truncated
        // encounter span move to clip-relative offsets.
        assert_eq!(entry.timeline.len(), 2);
        assert_eq!(entry.timeline[0].start_ms(), 20_000);
        assert_eq!(entry.timeline[1].start_ms(), 0);
        assert_eq!(entry.timeline[1].end_ms(), Some(25_000));

        assert!(entry.media_path.exists());
        assert!(source.media_path.exists(), "clip source must be retained");
        let sidecar: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&entry.sidecar_path).expect("sidecar"))
                .expect("json");
        assert_eq!(sidecar["schema_version"], SIDECAR_SCHEMA_VERSION);
        assert!(
            entry.media_path.to_string_lossy().contains("Clipped at "),
            "{}",
            entry.media_path.display()
        );

        let argv = harness.argv();
        assert!(argv.contains(&"-progress".to_owned()) && argv.contains(&"-nostats".to_owned()));
        assert!(
            argv.windows(2)
                .any(|pair| pair[0] == "-ss" && pair[1] == "10")
        );
        assert!(
            argv.windows(2)
                .any(|pair| pair[0] == "-t" && pair[1] == "25")
        );
        // Paths with spaces stay single arguments.
        assert!(argv.contains(&source.media_path.to_string_lossy().into_owned()));
        harness.shutdown_and_join();
        assert!(
            harness
                .staging()
                .read_dir()
                .expect("staging")
                .next()
                .is_some()
        );
    }

    /*
    #[test]
    fn kill_video_job_preserves_order_audio_progress_and_provenance() {
        let mut harness = Harness::new("kill");
        let first = harness.source("POV one", 60_000);
        let second = harness.source("POV two", 60_000);
        harness
            .jobs
            .send(MediaJob::CreateKillVideo {
                segments: vec![
                    KillSegment {
                        source: second.clone(),
                        start_ms: 5_000,
                        end_ms: 15_000,
                    },
                    KillSegment {
                        source: first.clone(),
                        start_ms: 1_000,
                        end_ms: 9_000,
                    },
                ],
                width: 1920,
                height: 1080,
                fps: 30,
                audio: KillAudio::Switched,
            })
            .expect("send kill video");

        let MediaEvent::Completed { kind, entry } = harness.outcome() else {
            panic!("kill video did not complete");
        };
        assert_eq!(kind, WorkKind::KillVideo);
        assert_eq!(entry.category, Category::Clip);
        assert_eq!(entry.duration_ms, 18_000);
        assert!(entry.protected);
        assert!(entry.title.contains("Multiview"));
        assert!(entry.title.contains("Chrome King Gallywix [M]"));
        assert_eq!(entry.media.width, Some(1920));
        let tag = entry.tag.clone().expect("provenance tag");
        assert!(tag.starts_with("WCR Multipov Kill Video."));
        assert!(tag.contains(&second.title) && tag.contains(&first.title));
        assert_eq!(
            entry.details,
            ActivityDetails::Clip {
                source_recording: second.id.clone(),
                source_category: Category::Raids,
                source_title: Some(second.title.clone()),
            }
        );

        let argv = harness.argv();
        let inputs: Vec<&String> = argv
            .windows(2)
            .filter(|pair| pair[0] == "-i")
            .map(|pair| &pair[1])
            .collect();
        assert_eq!(
            inputs,
            vec![
                &second.media_path.to_string_lossy().into_owned(),
                &first.media_path.to_string_lossy().into_owned()
            ]
        );
        let filter = argv
            .iter()
            .position(|arg| arg == "-filter_complex")
            .map(|index| argv[index + 1].clone())
            .expect("filter");
        assert!(filter.contains("[0:v]trim=start=5:end=15"));
        assert!(filter.contains("[1:v]trim=start=1:end=9"));
        assert!(filter.contains("concat=n=2:v=1:a=1[v][a]"));

        harness.shutdown_and_join();
    }

    #[test]
    fn kill_video_rejects_invalid_selections() {
        let harness = Harness::new("kill-invalid");
        let source = harness.source("POV one", 60_000);
        let single = vec![KillSegment {
            source: source.clone(),
            start_ms: 0,
            end_ms: 10_000,
        }];
        assert!(validate_kill_video(&single, 1920, 1080, 30, KillAudio::Switched).is_err());

        let duplicate = vec![
            KillSegment {
                source: source.clone(),
                start_ms: 0,
                end_ms: 10_000,
            },
            KillSegment {
                source: source.clone(),
                start_ms: 0,
                end_ms: 10_000,
            },
        ];
        assert!(validate_kill_video(&duplicate, 1920, 1080, 30, KillAudio::Switched).is_err());

        let other = harness.source("POV two", 60_000);
        let out_of_bounds = vec![
            KillSegment {
                source: source.clone(),
                start_ms: 0,
                end_ms: 10_000,
            },
            KillSegment {
                source: other.clone(),
                start_ms: 0,
                end_ms: 90_000,
            },
        ];
        assert!(validate_kill_video(&out_of_bounds, 1920, 1080, 30, KillAudio::Switched).is_err());

        let valid = vec![
            KillSegment {
                source,
                start_ms: 0,
                end_ms: 10_000,
            },
            KillSegment {
                source: other,
                start_ms: 0,
                end_ms: 10_000,
            },
        ];
        assert!(validate_kill_video(&valid, 0, 1080, 30, KillAudio::Switched).is_err());
        assert!(validate_kill_video(&valid, 1920, 1080, 30, KillAudio::Source(5)).is_err());
        assert!(validate_kill_video(&valid, 1920, 1080, 30, KillAudio::Switched).is_ok());
    }

    */
    fn finalize_job(harness: &Harness, with_replay: bool) -> MediaJob {
        let regular = harness.root.join("capture/regular/Video.mkv");
        fs::write(&regular, "regular bytes").expect("regular");
        let replay = with_replay.then(|| {
            let replay = harness.root.join("capture/replay/Replay.mkv");
            fs::write(&replay, "replay bytes").expect("replay");
            replay
        });
        MediaJob::FinalizeRecording {
            draft: Box::new(RecordingDraft {
                id: RecordingId::new(),
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
                player: None,
                combatants: Vec::new(),
                timeline: vec![TimelineItem::point(
                    TimelineKind::Death,
                    20_000,
                    Some("Testone".to_owned()),
                    Some(Outcome::Loss),
                    None,
                )],
                outcome: Some(Outcome::Win),
                ended_at_ms: Some(1_772_323_270_000),
                duration_ms: Some(85_000),
                title: Some("Testone - Chrome King Gallywix [M] (Kill)".to_owned()),
                activity_hash: Some("0f1e2d3c4b5a69788796a5b4c3d2e1f0".to_owned()),
            }),
            artifacts: CaptureArtifacts {
                replay,
                regular,
                requested_replay_ms: 8_000,
                regular_started_at_ms: 1_772_323_205_000,
                regular_stopped_at_ms: 1_772_323_275_000,
            },
            facts: MediaFacts {
                fps: Some(60),
                width: None,
                height: None,
                codec: Some(Codec::H264),
                has_content: true,
            },
        }
    }

    #[test]
    fn shutdown_cancels_maintenance_and_preserves_carried_finalization() {
        let harness = Harness::new("maintenance-shutdown");
        let logs = harness.root.join("logs");
        fs::create_dir_all(&logs).unwrap();
        fs::write(harness.library().join("legacy.mp4"), b"media").unwrap();
        fs::write(
            harness.library().join("legacy.json"),
            r#"{"category":"Mythic+","duration":120,"start":1784396963000}"#,
        )
        .unwrap();
        fs::write(logs.join("WoWCombatLog.txt"), "irrelevant\n".repeat(2048)).unwrap();

        let (_jobs, jobs_rx) = std::sync::mpsc::channel();
        let (control, control_rx) = sync_channel(1);
        let (events_tx, events) = std::sync::mpsc::channel();
        let storage = Storage::new(harness.library(), harness.root.join("capture"));
        let mut worker = MediaWorker::new(
            MediaConfig {
                ffmpeg: fake_ffmpeg(),
                utc_offset_minutes: 120,
                poll_interval: Duration::from_millis(20),
                finalize_grace: Duration::from_secs(5),
                sigint_grace: Duration::from_millis(400),
            },
            storage,
            jobs_rx,
            control_rx,
            events_tx,
        );
        control
            .send(MediaControl::Shutdown {
                pending_finalizations: vec![finalize_job(&harness, true)],
            })
            .unwrap();

        worker.run_job(MediaJob::EnrichLegacyBloodlust {
            retail_log_dirs: vec![logs],
            context: ParseTimeContext::new(2026, 120),
        });

        assert!(worker.shutdown_at.is_some());
        assert_eq!(worker.shutdown_finalizations.len(), 1);
        assert!(matches!(
            events.try_recv(),
            Ok(MediaEvent::TimelineEnriched { enriched: 0, .. })
        ));
    }

    #[test]
    fn finalization_trims_and_concatenates_the_replay() {
        let mut harness = Harness::new("finalize");
        harness
            .jobs
            .send(finalize_job(&harness, true))
            .expect("send");

        let MediaEvent::Completed { kind, entry } = harness.outcome() else {
            panic!("finalize did not complete");
        };
        assert_eq!(kind, WorkKind::Finalize);
        // The fake reports three seconds of usable replay in front of a
        // seventy-second regular recording.
        assert_eq!(entry.duration_ms, 73_000);
        assert_eq!(entry.timeline[0].start_ms(), 18_000);
        assert!(entry.media_path.exists());

        // The trim used -sseof with the requested lead-in and the concat used
        // the demuxer list; both intermediates are gone.
        let argv = harness.argv();
        assert!(
            argv.windows(2)
                .any(|pair| pair[0] == "-f" && pair[1] == "concat")
        );
        let leftovers: Vec<PathBuf> = fs::read_dir(harness.staging())
            .expect("staging")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) != Some("txt"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        harness.shutdown_and_join();
    }

    #[test]
    fn shutdown_processes_finalization_carried_outside_the_full_job_channel() {
        let mut harness =
            Harness::with_finalize_grace("shutdown-pending-finalize", Duration::from_secs(5));
        let pending = finalize_job(&harness, true);

        harness
            .control
            .send(MediaControl::Shutdown {
                pending_finalizations: vec![pending],
            })
            .expect("shutdown");

        let MediaEvent::Completed { kind, entry } = harness.outcome() else {
            panic!("pending finalization did not complete during shutdown");
        };
        assert_eq!(kind, WorkKind::Finalize);
        assert!(entry.media_path.exists());
        harness.worker.take().expect("worker").join().expect("join");
    }

    #[test]
    fn a_failing_trim_falls_back_to_the_regular_recording_alone() {
        let mut harness = Harness::new("finalize-fallback");
        harness.set_mode("fail");
        harness
            .jobs
            .send(finalize_job(&harness, true))
            .expect("send");

        let MediaEvent::Completed { entry, .. } = harness.outcome() else {
            panic!("fallback did not complete");
        };
        // Zero usable replay: the media starts with the regular recording and
        // the marker keeps its activity-relative distance minus the lead-in.
        assert_eq!(entry.duration_ms, 70_000);
        assert_eq!(entry.timeline[0].start_ms(), 15_000);
        assert_eq!(
            fs::read_to_string(&entry.media_path).expect("media"),
            "regular bytes"
        );
        harness.shutdown_and_join();
    }

    #[test]
    fn shutdown_terminates_a_silent_child_and_leaves_no_process() {
        let mut harness = Harness::new("shutdown-silent");
        harness.set_mode("silent");
        let source = harness.source("Raid POV", 60_000);
        harness
            .jobs
            .send(MediaJob::CreateClip {
                source: Box::new(source),
                start_ms: 0,
                end_ms: 30_000,
            })
            .expect("send clip");

        // Let the child start before asking for shutdown.
        thread::sleep(Duration::from_millis(200));
        harness
            .control
            .send(MediaControl::Shutdown {
                pending_finalizations: Vec::new(),
            })
            .expect("shutdown");
        let event = harness.outcome();
        assert_eq!(
            event,
            MediaEvent::Cancelled {
                kind: WorkKind::Clip
            }
        );

        let pattern = harness.staging().to_string_lossy().into_owned();
        harness.worker.take().expect("worker").join().expect("join");
        assert!(!any_running(&pattern), "an FFmpeg child survived shutdown");
        // Nothing was published: only the untouched source media remains.
        let published: Vec<PathBuf> = fs::read_dir(harness.library())
            .expect("library")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect();
        assert_eq!(published.len(), 1, "{published:?}");
        assert_eq!(
            published[0].extension().and_then(|value| value.to_str()),
            Some("mp4")
        );
    }

    #[test]
    fn a_dropped_control_channel_still_interrupts_finalization() {
        // A nonzero grace proves the deadline is fixed once: if every
        // disconnected poll pushed it forward, the silent child would never
        // be interrupted and this test would time out.
        let mut harness =
            Harness::with_finalize_grace("shutdown-disconnect", Duration::from_millis(500));
        harness.set_mode("silent");
        harness
            .jobs
            .send(finalize_job(&harness, true))
            .expect("send");

        // Let the child start, then drop the sender (coordinator death). That
        // must behave like shutdown: the grace deadline is fixed once, not
        // pushed forward by every disconnected poll.
        thread::sleep(Duration::from_millis(200));
        drop(std::mem::replace(&mut harness.control, sync_channel(1).0));
        let event = harness.outcome();
        assert_eq!(
            event,
            MediaEvent::Cancelled {
                kind: WorkKind::Finalize
            }
        );

        let pattern = harness.staging().to_string_lossy().into_owned();
        harness.worker.take().expect("worker").join().expect("join");
        assert!(!any_running(&pattern), "an FFmpeg child survived shutdown");
    }

    #[test]
    fn shutdown_terminates_a_chatty_child_and_reads_a_bounded_log_tail() {
        let mut harness = Harness::new("shutdown-chatty");
        harness.set_mode("chatty");
        let source = harness.source("Raid POV", 60_000);
        harness
            .jobs
            .send(MediaJob::CreateClip {
                source: Box::new(source),
                start_ms: 0,
                end_ms: 30_000,
            })
            .expect("send clip");

        thread::sleep(Duration::from_millis(200));
        // Progress is parsed incrementally while the child is still writing.
        assert!(
            harness
                .progress()
                .iter()
                .any(|progress| progress.completed > 0)
        );

        harness
            .control
            .send(MediaControl::Shutdown {
                pending_finalizations: Vec::new(),
            })
            .expect("shutdown");
        let event = harness.outcome();
        assert_eq!(
            event,
            MediaEvent::Cancelled {
                kind: WorkKind::Clip
            }
        );

        let pattern = harness.staging().to_string_lossy().into_owned();
        harness.worker.take().expect("worker").join().expect("join");
        assert!(!any_running(&pattern), "an FFmpeg child survived shutdown");
    }

    #[test]
    fn a_failed_job_reports_the_ffmpeg_log_tail() {
        let mut harness = Harness::new("failure");
        harness.set_mode("fail");
        let source = harness.source("Raid POV", 60_000);
        harness
            .jobs
            .send(MediaJob::CreateClip {
                source: Box::new(source),
                start_ms: 0,
                end_ms: 30_000,
            })
            .expect("send clip");

        let MediaEvent::Failed { kind, message } = harness.outcome() else {
            panic!("clip did not fail");
        };
        assert_eq!(kind, WorkKind::Clip);
        assert!(message.contains("deliberate failure detail"), "{message}");
        assert!(message.len() <= 8 * 1024 + 200);
        // The claimed output name is released again.
        let published: Vec<PathBuf> = fs::read_dir(harness.library())
            .expect("library")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("mp4"))
            .collect();
        assert_eq!(published.len(), 1, "{published:?}");
        harness.shutdown_and_join();
    }

    #[test]
    fn local_stamps_and_clip_ranges_are_deterministic() {
        assert_eq!(
            format_local_stamp(1_772_323_200_000, 0),
            "2026-03-01 00-00-00"
        );
        assert_eq!(
            format_local_stamp(1_772_323_200_000, 120),
            "2026-03-01 02-00-00"
        );
        assert_eq!(format_local_stamp(0, -60), "1969-12-31 23-00-00");
        assert_eq!(format_seconds(1_500), "1.5");
        assert_eq!(format_seconds(30_000), "30");
    }
}
