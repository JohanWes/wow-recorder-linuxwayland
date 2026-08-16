// SPDX-License-Identifier: GPL-3.0-or-later

//! Development tool: replay a retail combat log through the production parser
//! and activity engine, then print the meter of one finished recording the way
//! `storage::finalize` would persist it (media-relative, shifted by lead-in).
//!
//! Usage:
//! ```text
//! cargo run --release --example replay_meter -- \
//!   <combat log> <window start prefix> <window end prefix> \
//!   <activity start epoch ms> <lead-in ms> <media duration ms>
//! ```
//!
//! The window prefixes are raw log timestamps such as `8/11/2026 20:38:17`;
//! lines compare by string, so they must match the log's own format. The
//! activity start selects the recording (it equals the persisted
//! `start_unix_ms`), and the lead-in is `activity start - media start`.

use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::process::ExitCode;

use warcraft_recorder::activity::{ActivityAction, ActivityEngine};
use warcraft_recorder::config::ActivitySettings;
use warcraft_recorder::domain::GameFlavor;
use warcraft_recorder::parser::{ParseTimeContext, parse_line};
use warcraft_recorder::storage::shift_meter;

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    let (Some(log_path), Some(window_start), Some(window_end)) =
        (args.next(), args.next(), args.next())
    else {
        eprintln!(
            "usage: replay_meter <log> <window start> <window end> \
             <activity start ms> <lead-in ms> <media duration ms>"
        );
        return ExitCode::FAILURE;
    };
    let (Some(activity_start_ms), Some(lead_in_ms), Some(duration_ms)) =
        (args.next(), args.next(), args.next())
    else {
        eprintln!(
            "usage: replay_meter <log> <window start> <window end> \
             <activity start ms> <lead-in ms> <media duration ms>"
        );
        return ExitCode::FAILURE;
    };
    let activity_start_ms: i64 = match activity_start_ms.parse() {
        Ok(value) => value,
        Err(_) => {
            eprintln!("invalid activity start ms");
            return ExitCode::FAILURE;
        }
    };
    let lead_in_ms: i64 = match lead_in_ms.parse() {
        Ok(value) => value,
        Err(_) => {
            eprintln!("invalid lead-in ms");
            return ExitCode::FAILURE;
        }
    };
    let duration_ms: u64 = match duration_ms.parse() {
        Ok(value) => value,
        Err(_) => {
            eprintln!("invalid media duration ms");
            return ExitCode::FAILURE;
        }
    };

    let file = match File::open(&log_path) {
        Ok(file) => file,
        Err(error) => {
            eprintln!("cannot open {log_path}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut reader = BufReader::new(file);

    // The combat log version selects the advanced-block arity; it is written
    // in the first lines of every session file.
    let mut context = ParseTimeContext::new(2026, 120);
    let mut header = String::new();
    for _ in 0..20 {
        header.clear();
        if reader.read_line(&mut header).unwrap_or(0) == 0 {
            break;
        }
        if let Some(version) = header
            .split_once("COMBAT_LOG_VERSION,")
            .and_then(|(_, rest)| rest.split(',').next())
            .and_then(|value| value.parse::<u32>().ok())
        {
            context = context.with_combat_log_version(version);
            break;
        }
    }

    let mut engine = ActivityEngine::new();
    let config = ActivitySettings::default();
    let mut starts: HashMap<warcraft_recorder::domain::RecordingId, i64> = HashMap::new();
    let mut fed = 0u64;

    for line in reader.lines() {
        let line = match line {
            Ok(line) => line,
            Err(error) => {
                eprintln!("read error: {error}");
                return ExitCode::FAILURE;
            }
        };
        let prefix = &line[..window_start.len().min(line.len())];
        if prefix < window_start.as_str() {
            continue;
        }
        if prefix > window_end.as_str() {
            break;
        }
        let Ok(Some(event)) = parse_line(GameFlavor::Retail, context, &line) else {
            continue;
        };
        fed += 1;
        for action in engine.handle(event, &config) {
            match action {
                ActivityAction::Begin { draft, .. } => {
                    starts.insert(draft.id.clone(), draft.started_at_ms);
                }
                ActivityAction::Complete { id, .. }
                    if starts.get(&id) == Some(&activity_start_ms) =>
                {
                    let Some(draft) = engine.take_finished(&id) else {
                        eprintln!("finished draft for {id} is missing");
                        return ExitCode::FAILURE;
                    };
                    let media_start_ms = draft.started_at_ms - lead_in_ms;
                    let meter = shift_meter(
                        &draft.meter,
                        draft.started_at_ms,
                        media_start_ms,
                        duration_ms,
                    );
                    eprintln!(
                        "replayed {fed} events; {} ({:?}), {}-{} media ms",
                        draft.title.clone().unwrap_or_default(),
                        draft.outcome.unwrap(),
                        media_start_ms,
                        media_start_ms + duration_ms as i64,
                    );
                    println!("{}", serde_json::to_string_pretty(&meter).unwrap());
                    return ExitCode::SUCCESS;
                }
                _ => {}
            }
        }
    }
    eprintln!("no finished recording with start_unix_ms {activity_start_ms} in the window");
    ExitCode::FAILURE
}
